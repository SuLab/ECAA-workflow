#!/usr/bin/env python3
"""measure_de_effect_size.py - deterministic effect-size-reliability metric.

Shipped into an emitted package's lib/ and run verbatim inside the bio-min
container by the agent (gated by the task spec's attributes.measurement_script
flag). Parses the agent's OWN differential-expression results table and emits
result.json keys the validation-contract numeric_threshold assertion reads. No
host compute; runs in the container. The reference bounds are operator-authored
constants here and are NEVER handed to the agent.

DOMAIN-CORRECTNESS FACT (method-neutral). This recomputes a single scale-free
scalar from the agent's own results table: the typical (median) abundance of the
agent's OWN top features ranked by absolute effect size, AS A RATIO to the
typical abundance over the whole tested set. Under independence (effect ⟂
abundance) the top-K is a random sample of the set, so the ratio sits near 1; an
extreme effect estimate concentrated in the lowest-abundance features drives the
ratio toward 0 — that ranked output is unreliable as a "strongest finding". The
script states that property of the agent's ranked output and prescribes no
remedy. How to address it (re-rank, weight, defend the calls, or otherwise) is
the agent's choice; this script names no method, estimator, filter, or threshold
value to the agent.

A RATIO rather than a count is used deliberately: a count of top features below
a bottom-quartile cut over the FULL table is dominated by unexpressed genes (the
quartile cut sits below the low-count hits, so they are NOT flagged), so it does
not fire on the very artifact it targets. The ratio is null-robust: it compares
the top set's typical abundance directly against the tested set's typical
abundance, so it is ≈1 under independence and ≈0 for the low-count artifact,
independent of how many unexpressed rows pad the table.

The check self-gates: when the agent's table carries no abundance/information
column, information_column_recorded is false and the contract assertion's `when`
clause skips (never false-fails). An abundance column is the precondition for
the check, NOT a prescribed output.

REPORT-COMPLETENESS FACTS (method-neutral, da-8-1 C8). When the agent's OWN
results table records a model-fit / variance-explained column (R-squared and
synonyms) or a per-row sample-size column, the agent's written answer should
SURFACE that statistic — a domain-correctness fact about the agent's own
recorded output, prescribing no method/estimator/threshold. This script emits
two boolean presence flags with the SAME recorded-ness semantics as
information_column_recorded (column FOUND in the header AND >=1 usable value):
r_squared_column_recorded and sample_size_column_recorded. It also folds the
task's sibling narrative artifact(s) (report.md / interpretation.md /
summary.md / answer.txt and any other .md/.txt, plus a pre-existing
result.json `narrative` field) into a single deterministic narrative_text key
so the contract's string_contains has ONE channel to read — the agent's answer
may live in result.json OR in a sibling .md/.txt, and reading only one would
false-fire when the answer sits in the other. When a model-fit / per-row-n
column is absent the flag is false and the downstream assertion's `when` clause
SKIPS (never blocks, never prescribes producing the column).

SELF-DESCRIBING METRIC (method-neutral). A bare ratio handed to a narrating
agent has no denominator attached, and a deposited report duly invented one: it
attributed `top_effect_abundance_ratio` to "the median baseMean of all 4,030
significant genes" when the denominator is the median over ALL 22,369 TESTED
features, and a sibling report named BOTH populations inside one sentence. So
alongside the bare number this emits `top_effect_abundance_ratio_basis` (machine
-readable: which population, which statistic, which column, and how large each
population was) and `top_effect_abundance_ratio_description` (one quotable
sentence). The narrative is required to QUOTE the description rather than
paraphrase the population. It also records how many of the top-K carry no usable
significance value — in the deposited run 4 of the top 15 had `padj = NA`, so the
top set was not even a subset of the significant set the prose named.

bio-min has python3 stdlib; this reads a TSV with the stdlib csv module (no
pandas/numpy dependency), gz-aware.
"""
import argparse
import csv
import gzip
import io
import json
import os
import sys

# Operator-authored reference bounds. SME-overridable by editing this pinned
# script; never passed to the agent as a threshold.
#
# TOP_K               rank the agent's own top-K features by |effect size|.
# MIN_ABUNDANCE_RATIO null-robust (≈1 under independence); the da-15-1
#                 unshrunken-low-count artifact measures ≈0.09 over the tested
#                 set (0.006 over the significant set); a legitimately mild
#                 low-abundance biological signal stays well above 0.20;
#                 SME-overridable by editing this pinned script; names NO method.
#                 The metric is the ratio of the median abundance of the agent's
#                 OWN top-K-by-|effect| features to the median abundance over the
#                 whole tested set (a quantity RELATIVE to the agent's own table,
#                 not an absolute value handed to the agent — so it holds across
#                 abundance-column conventions without naming an axis value).
TOP_K = 15
MIN_ABUNDANCE_RATIO = 0.20

# Semantic-role column detection, keyed on role not header. Mirrors
# lib/plotting/stages/differential_expression.py so the common results-table
# header conventions (base_mean / logCPM / AveExpr abundance columns; log2fc /
# logFC effect columns) all resolve without naming or branching on any analysis
# method. Matched case-insensitively.
_EFFECT_COLS = ("log2fc", "log2foldchange", "logfc", "lfc", "log2_fc")
_BASEMEAN_COLS = (
    "base_mean", "basemean", "mean_expression", "mean_expr",
    "ave_expr", "aveexpr", "logcpm", "avelogcpm",
)
_ADJ_COLS = ("adj_pvalue", "padj", "fdr", "adj_p")
_P_COLS = ("pvalue", "p_value", "p", "pval")
# Significance role, adjusted-first: a top-K feature with no usable value here
# is not a member of any significant set, which is what makes the count of such
# features material to prose about "significant" features.
_SIGNIFICANCE_COLS = _ADJ_COLS + _P_COLS

# Model-fit / variance-explained role (R-squared and its conventional header
# synonyms). Detects the STATISTIC ROLE by header convention exactly as
# _EFFECT_COLS / _BASEMEAN_COLS do — it names NO model, test, or estimator; the
# agent chose to compute and tabulate these columns.
_MODELFIT_COLS = (
    "r_squared", "r2", "rsquared", "r_sq", "rsq",
    "adj_r_squared", "adj_r2", "adjusted_r_squared", "pseudo_r2",
)
# Per-row sample-size role.
_SAMPLESIZE_COLS = (
    "n", "n_samples", "sample_size", "num_samples", "n_obs", "nobs", "n_used",
)

# Narrative-artifact filenames, in the SAME precedence as the in-tree
# claim_verifier::find_narrative_artifact (report > interpretation > summary >
# other). The string_contains contract reads ONE file (result.json), so the
# measurement folds these siblings into result.json's narrative_text — without
# this the check would false-fire when the answer sits in a sibling .md/.txt.
_NARRATIVE_EXTS = (".md", ".txt")


def _find_col(header, candidates):
    """Index of the first candidate present in header (case-insensitive role
    match), or None. Pure + deterministic."""
    lower = [h.strip().lower() for h in header]
    for cand in candidates:
        try:
            return lower.index(cand.lower())
        except ValueError:
            continue
    return None


def _column_recorded(header, rows, candidates):
    """True iff a role column from `candidates` is FOUND in the header AND
    yields >=1 usable (parseable float) value over the rows. Same recorded-ness
    semantics as information_column_recorded: a header-only / all-NaN column
    reads False so the downstream `when` gate SKIPS rather than firing on an
    empty basis. Pure + deterministic; names no method."""
    idx = _find_col(header, candidates)
    if idx is None:
        return False
    for row in rows:
        if idx >= len(row):
            continue
        try:
            float(row[idx])
        except (ValueError, TypeError):
            continue
        return True
    return False


def _fold_narrative_text(out_path):
    """Fold the task's sibling narrative artifact(s) and a pre-existing
    result.json `narrative` field into a single deterministic text blob.

    `out_path` is the result.json the script writes; its PARENT directory is
    THIS task's output dir (runtime/outputs/<task_id>/), which also holds the
    agent's narrative .md/.txt deliverable. We collect every .md/.txt sibling
    (sorted by find_narrative_artifact precedence, then name, for determinism)
    PLUS any `narrative` string already in result.json, and concatenate them so
    the contract's string_contains has ONE channel to read. Best-effort: an
    unreadable file is skipped (never raises). Pure w.r.t. inputs; deterministic
    ordering.

    Returns the folded text (possibly empty)."""

    def _precedence(name):
        low = name.lower()
        if "report" in low:
            return 0
        if "interpretation" in low:
            return 1
        if "summary" in low:
            return 2
        return 3

    parts = []
    out_dir = os.path.dirname(os.path.abspath(out_path))
    # Own narrative: a `narrative` string in a pre-existing result.json.
    try:
        with open(out_path, "rt") as fh:
            prior = json.load(fh)
        if isinstance(prior, dict):
            nval = prior.get("narrative")
            if isinstance(nval, str) and nval:
                parts.append(nval)
    except (OSError, ValueError):
        pass
    # Sibling narrative artifacts (.md/.txt), precedence-then-name ordered.
    names = []
    try:
        names = sorted(os.listdir(out_dir))
    except OSError:
        names = []
    siblings = [
        n for n in names
        if os.path.splitext(n)[1].lower() in _NARRATIVE_EXTS
        and n.lower() != "result.json"
    ]
    siblings.sort(key=lambda n: (_precedence(n), n.lower()))
    for n in siblings:
        try:
            with open(os.path.join(out_dir, n), "rt") as fh:
                parts.append(fh.read())
        except OSError:
            continue
    return "\n".join(parts)


#: Machine-readable identity of the ratio's two populations and its statistic.
#: These are IDs, not prose: a downstream reader keys on them, and the prose it
#: quotes is the emitted description.
RATIO_STATISTIC_ID = "ratio_of_medians_over_features"
NUMERATOR_POPULATION_ID = "top_k_tested_features_by_absolute_effect"
DENOMINATOR_POPULATION_ID = "all_tested_features"

#: Role labels used when the caller supplies no column names, so a description
#: is never silently modality-specific.
_GENERIC_EFFECT_LABEL = "effect estimate"
_GENERIC_INFORMATION_LABEL = "abundance/information value"
_GENERIC_SIGNIFICANCE_LABEL = "significance value"


def _usable_float(raw):
    """Parsed float, or None when the cell is absent, unparseable, or NaN.

    Used ONLY for the significance role. The pre-existing abundance/effect
    parsing deliberately keeps its bare `float()` semantics so
    information_column_recorded is unchanged; here a NaN must read as "no
    usable value" because that is exactly the `padj = NA` case the count
    exists to surface."""
    try:
        value = float(raw)
    except (ValueError, TypeError):
        return None
    return value if value == value else None


def _label(name, fallback):
    """The run's OWN column name when it is known, else the generic role word.

    Backticked so a report can lift the description verbatim and the column
    name reads as an identifier rather than as prose."""
    return f"`{name}`" if name else fallback


def abundance_ratio_basis(
    *,
    computed,
    ratio,
    requested_top_k,
    numerator_population_size,
    denominator_population_size,
    missing_significance_count,
    column_names=None,
    not_computed_reason=None,
):
    """The self-describing companion to `top_effect_abundance_ratio`.

    Returns `(basis, description)`.

    `basis` is machine-readable and always carries the SAME key set, so a
    downstream reader never has to branch on presence: which population sat in
    the numerator and which in the denominator, how large each was, which of the
    agent's own columns supplied the effect / abundance / significance role, and
    how many of the top-K carry no usable significance value.

    `description` states the same thing in prose, written to be QUOTED verbatim
    by a narrative. Naming a different population than this description names is
    a reporting error, so the description states the denominator positively AND
    rules out the populations reports have wrongly substituted for it (a
    significant subset, a per-sample statistic). It deliberately does NOT contain
    the words a report must not use for this statistic, so quoting it verbatim
    cannot trip the in-tree prose check.

    `computed=False` is the gate-skip / degenerate path: the recorded
    `top_effect_abundance_ratio` is then a non-failing sentinel rather than a
    measurement, and the description says so instead of describing a population
    that was never used. Pure + deterministic; names no method."""
    names = column_names or {}
    effect_label = _label(names.get("effect"), _GENERIC_EFFECT_LABEL)
    info_label = _label(names.get("information"), _GENERIC_INFORMATION_LABEL)
    sig_label = _label(names.get("significance"), _GENERIC_SIGNIFICANCE_LABEL)

    basis = {
        "computed": bool(computed),
        "denominator": None,
        "denominator_population": DENOMINATOR_POPULATION_ID,
        "denominator_population_size": int(denominator_population_size),
        "denominator_statistic": "median",
        "effect_column": names.get("effect"),
        "information_column": names.get("information"),
        "not_computed_reason": not_computed_reason,
        "numerator": None,
        "numerator_population": NUMERATOR_POPULATION_ID,
        "numerator_population_size": int(numerator_population_size),
        "numerator_statistic": "median",
        "requested_top_k": int(requested_top_k),
        "significance_column": names.get("significance"),
        "statistic": RATIO_STATISTIC_ID,
        "top_k_missing_significance_count": missing_significance_count,
        "value": None,
    }

    if not computed:
        reason = not_computed_reason or "the ratio was not computed"
        description = (
            f"top_effect_abundance_ratio was NOT computed for this run ({reason}). "
            "The recorded value is a non-failing sentinel, not a measurement: do not "
            "cite it, and do not describe a population for it."
        )
        return basis, description

    num_n = int(numerator_population_size)
    den_n = int(denominator_population_size)
    basis["value"] = float(ratio)
    basis["numerator"] = (
        f"median {info_label} over the {num_n} tested features with the largest "
        f"|{effect_label}|"
    )
    basis["denominator"] = f"median {info_label} over all {den_n} tested features"

    # The wording deliberately avoids the words a report must NOT use for this
    # statistic ("mean"/"average"): the description is meant to be QUOTED
    # verbatim, and the in-tree prose check warns on either word appearing near a
    # ratio citation. It states the correct terms positively instead; the
    # prohibition lives in the agent prompt and in that check.
    description = (
        f"top_effect_abundance_ratio = {ratio:.4f} is the MEDIAN {info_label} over the "
        f"{num_n} tested features with the largest |{effect_label}|, divided by the "
        f"MEDIAN {info_label} over ALL {den_n} tested features. Both terms are medians "
        "and both are computed over FEATURES, so the ratio is not a per-sample "
        "statistic; its denominator is the whole tested set — not a significant subset "
        "and not any other population."
    )
    if missing_significance_count is None:
        description += (
            " No significance column was recorded, so this metric supports no "
            "statement about a significant subset."
        )
    else:
        description += (
            f" {int(missing_significance_count)} of those {num_n} top features carry no "
            f"usable {sig_label}."
        )
        if int(missing_significance_count) > 0:
            description += (
                " The top set is therefore NOT a subset of the significant set, and no "
                "sentence may describe it as one."
            )
    return basis, description


def _median(sorted_values):
    """Standard median of an already-sorted ascending list. Averages the two
    middle elements for an even-length list. Returns None for an empty list.
    Pure + deterministic."""
    n = len(sorted_values)
    if n == 0:
        return None
    mid = n // 2
    if n % 2 == 1:
        return sorted_values[mid]
    return (sorted_values[mid - 1] + sorted_values[mid]) / 2.0


def compute_metrics(
    rows,
    effect_idx,
    info_idx,
    top_k=TOP_K,
    *,
    sig_idx=None,
    column_names=None,
):
    """Pure metric core (unit-tested directly).

    `rows`: list of parsed rows; each row is the list of string cells.
    `effect_idx`: column index of the effect-size estimate (|effect| is ranked).
    `info_idx`: column index of the per-feature abundance/information value, or
                None when the table has no such column.
    `sig_idx`: column index of the per-feature significance value, or None when
               the table has no such column. Used ONLY to count how many of the
               top-K carry no usable value there; it never enters the ratio and
               never filters the tested set.
    `column_names`: optional `{"effect", "information", "significance"}` map of
               the run's OWN resolved header names, so the emitted description
               names the columns the agent actually recorded.

    Returns a dict with:
      information_column_recorded: bool — whether an abundance column was found
        AND yielded usable numeric values. The contract assertion gates on this:
        false -> the check is SKIPPED (never false-fails a table with no
        abundance basis). It is a precondition signal, not a prescribed output.
      top_effect_abundance_ratio: float — the median abundance of the agent's
        own top-K features by |effect| DIVIDED BY the median abundance over the
        tested set (all rows carrying a usable numeric effect AND abundance
        value). Null-robust: ≈1 under independence (effect ⟂ abundance), driven
        toward 0 by the unshrunken-low-count artifact. Recomputed entirely from
        the agent's own table; compared by the contract against an
        operator-authored bound the agent never sees.
        When information_column_recorded is False the assertion is `when`-gated
        away, so this value is irrelevant; it is recorded as the neutral,
        non-failing sentinel 1.0 and the gate (not this value) skips the check.
      top_effect_abundance_ratio_basis / top_effect_abundance_ratio_description:
        the self-describing companions (see `abundance_ratio_basis`). Additive —
        the numeric keys above are unchanged.
      top_effect_k / tested_feature_count: informational only.
    """
    # Parse (|effect|, info, significance) triples, dropping rows where the
    # effect is unparseable. `sig` rides along only to be counted over the top-K.
    pairs = []
    for row in rows:
        if effect_idx >= len(row):
            continue
        try:
            eff = abs(float(row[effect_idx]))
        except (ValueError, TypeError):
            continue
        info = None
        if info_idx is not None and info_idx < len(row):
            try:
                info = float(row[info_idx])
            except (ValueError, TypeError):
                info = None
        sig = None
        if sig_idx is not None and sig_idx < len(row):
            sig = _usable_float(row[sig_idx])
        pairs.append((eff, info, sig))

    # The tested set is every row carrying BOTH a usable effect and a usable
    # abundance value. An abundance column "recorded" means we both FOUND a
    # candidate column and extracted at least one such usable pair; a
    # header-only / all-NaN column reads as not-recorded so the gate skips
    # rather than dividing on an empty set.
    tested = [triple for triple in pairs if triple[1] is not None]
    information_column_recorded = info_idx is not None and len(tested) > 0

    def _skipped(reason, recorded, count):
        """The gate-skip / degenerate result: the sentinel ratio plus a basis
        block that says plainly it is not a measurement."""
        basis, description = abundance_ratio_basis(
            computed=False,
            ratio=None,
            requested_top_k=top_k,
            numerator_population_size=0,
            denominator_population_size=count,
            missing_significance_count=None,
            column_names=column_names,
            not_computed_reason=reason,
        )
        return {
            "information_column_recorded": recorded,
            "top_effect_abundance_ratio": 1.0,
            "top_effect_abundance_ratio_basis": basis,
            "top_effect_abundance_ratio_description": description,
            "top_effect_k": int(top_k),
            "tested_feature_count": count,
        }

    if not information_column_recorded:
        # Gate-skip path: the assertion is `when`-gated on
        # information_column_recorded, so this ratio is never consulted. Record
        # the neutral, non-failing sentinel and rely on the gate.
        return _skipped(
            "no usable per-feature abundance/information column was recorded",
            False,
            len(pairs),
        )

    set_info_sorted = sorted(triple[1] for triple in tested)
    set_median = _median(set_info_sorted)
    # Degenerate set median (0): the abundance column cannot discriminate, so we
    # cannot form a meaningful ratio. Record the neutral 1.0 rather than divide
    # by zero or false-fail.
    if set_median is None or set_median == 0:
        return _skipped(
            "the median abundance over the tested set is 0, so no ratio can be formed",
            True,
            len(tested),
        )

    # The agent's own top-K by |effect| (descending). Stable order: |effect|
    # desc, then ascending info so ties are deterministic.
    ranked = sorted(tested, key=lambda p: (-p[0], p[1]))
    top = ranked[: int(top_k)]
    top_info_sorted = sorted(triple[1] for triple in top)
    top_median = _median(top_info_sorted)
    ratio = top_median / set_median
    # None when the table records no significance column at all: 0 would read as
    # "every top feature has one", which is a different and unsupported claim.
    missing_significance = (
        None if sig_idx is None else sum(1 for triple in top if triple[2] is None)
    )
    basis, description = abundance_ratio_basis(
        computed=True,
        ratio=ratio,
        requested_top_k=top_k,
        numerator_population_size=len(top),
        denominator_population_size=len(tested),
        missing_significance_count=missing_significance,
        column_names=column_names,
    )
    return {
        "information_column_recorded": True,
        "top_effect_abundance_ratio": float(ratio),
        "top_effect_abundance_ratio_basis": basis,
        "top_effect_abundance_ratio_description": description,
        "top_effect_k": int(top_k),
        "tested_feature_count": len(tested),
    }


def report_completeness_flags(header, rows):
    """Pure: two report-completeness presence flags over the agent's OWN table
    (unit-tested directly). Mirrors information_column_recorded EXACTLY — each
    flag is True iff the role column is FOUND in the header AND yields >=1 usable
    value over the rows; a header-only / all-NaN column reads False so the
    downstream `when` gate SKIPS (never blocks, never prescribes the column).

      r_squared_column_recorded: a model-fit / variance-explained column
        (R-squared and synonyms) is recorded.
      sample_size_column_recorded: a per-row sample-size column is recorded.

    Detects the STATISTIC ROLE by header convention only; names no method,
    estimator, test, or threshold value. Deterministic."""
    return {
        "r_squared_column_recorded": _column_recorded(header, rows, _MODELFIT_COLS),
        "sample_size_column_recorded": _column_recorded(header, rows, _SAMPLESIZE_COLS),
    }


def write_result(metrics, out_path):
    with open(out_path, "w") as fh:
        json.dump(metrics, fh, sort_keys=True, indent=2)
        fh.write("\n")


def _read_table(table_path):
    """Read a (gz-aware) TSV into (header, rows). Returns (None, []) when the
    file can't be read."""
    opener = gzip.open if str(table_path).endswith(".gz") else open
    try:
        with opener(table_path, "rt", newline="") as fh:
            text = fh.read()
    except OSError:
        return (None, [])
    reader = csv.reader(io.StringIO(text), delimiter="\t")
    rows = list(reader)
    if not rows:
        return (None, [])
    return (rows[0], rows[1:])


def main(argv=None):
    parser = argparse.ArgumentParser(
        description="DE effect-size-reliability measurement (container-run)"
    )
    parser.add_argument(
        "--table", required=True,
        help="path to THIS task's DE results table (de_results.tsv[.gz])",
    )
    parser.add_argument("--out", required=True, help="path to write result.json")
    args = parser.parse_args(argv)
    header, rows = _read_table(args.table)
    if header is None:
        # No readable table -> emit a not-recorded result so the gate skips
        # rather than fail-closing on a missing metric. The report-completeness
        # flags are also false (no header to detect a role column) and narrative
        # folding still runs (so a sibling-answer channel is captured).
        basis, description = abundance_ratio_basis(
            computed=False,
            ratio=None,
            requested_top_k=TOP_K,
            numerator_population_size=0,
            denominator_population_size=0,
            missing_significance_count=None,
            not_computed_reason="no readable results table was found",
        )
        result = {
            "information_column_recorded": False,
            "top_effect_abundance_ratio": 1.0,
            "top_effect_abundance_ratio_basis": basis,
            "top_effect_abundance_ratio_description": description,
            "top_effect_k": int(TOP_K),
            "tested_feature_count": 0,
            "r_squared_column_recorded": False,
            "sample_size_column_recorded": False,
        }
        result["narrative_text"] = _fold_narrative_text(args.out)
        write_result(result, args.out)
        return 0
    # Report-completeness flags are computed from the header/rows whenever a
    # table exists — even when there is no effect column to rank (a table can
    # still record R-squared / per-row n).
    completeness = report_completeness_flags(header, rows)
    effect_idx = _find_col(header, _EFFECT_COLS)
    info_idx = _find_col(header, _BASEMEAN_COLS)
    sig_idx = _find_col(header, _SIGNIFICANCE_COLS)
    # The run's OWN header names, so the emitted description names the columns
    # the agent actually recorded rather than a generic role word.
    column_names = {
        "effect": header[effect_idx] if effect_idx is not None else None,
        "information": header[info_idx] if info_idx is not None else None,
        "significance": header[sig_idx] if sig_idx is not None else None,
    }
    if effect_idx is None:
        # No effect-size column at all -> nothing to rank; the abundance-ratio
        # gate skips, but the report-completeness flags + narrative still apply.
        basis, description = abundance_ratio_basis(
            computed=False,
            ratio=None,
            requested_top_k=TOP_K,
            numerator_population_size=0,
            denominator_population_size=0,
            missing_significance_count=None,
            column_names=column_names,
            not_computed_reason="the results table records no per-feature effect-size column",
        )
        result = {
            "information_column_recorded": False,
            "top_effect_abundance_ratio": 1.0,
            "top_effect_abundance_ratio_basis": basis,
            "top_effect_abundance_ratio_description": description,
            "top_effect_k": int(TOP_K),
            "tested_feature_count": 0,
        }
        result.update(completeness)
        result["narrative_text"] = _fold_narrative_text(args.out)
        write_result(result, args.out)
        return 0
    metrics = compute_metrics(
        rows, effect_idx, info_idx, sig_idx=sig_idx, column_names=column_names
    )
    metrics.update(completeness)
    metrics["narrative_text"] = _fold_narrative_text(args.out)
    write_result(metrics, args.out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
