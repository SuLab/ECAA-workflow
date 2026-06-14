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

bio-min has python3 stdlib; this reads a TSV with the stdlib csv module (no
pandas/numpy dependency), gz-aware.
"""
import argparse
import csv
import gzip
import io
import json
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


def compute_metrics(rows, effect_idx, info_idx, top_k=TOP_K):
    """Pure metric core (unit-tested directly).

    `rows`: list of parsed rows; each row is the list of string cells.
    `effect_idx`: column index of the effect-size estimate (|effect| is ranked).
    `info_idx`: column index of the per-feature abundance/information value, or
                None when the table has no such column.

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
      top_effect_k / significant_feature_count: informational only.
    """
    # Parse (|effect|, info) pairs, dropping rows where either is unparseable.
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
        pairs.append((eff, info))

    # The tested set is every row carrying BOTH a usable effect and a usable
    # abundance value. An abundance column "recorded" means we both FOUND a
    # candidate column and extracted at least one such usable pair; a
    # header-only / all-NaN column reads as not-recorded so the gate skips
    # rather than dividing on an empty set.
    tested = [(eff, info) for (eff, info) in pairs if info is not None]
    information_column_recorded = info_idx is not None and len(tested) > 0

    if not information_column_recorded:
        # Gate-skip path: the assertion is `when`-gated on
        # information_column_recorded, so this ratio is never consulted. Record
        # the neutral, non-failing sentinel and rely on the gate.
        return {
            "information_column_recorded": False,
            "top_effect_abundance_ratio": 1.0,
            "top_effect_k": int(top_k),
            "significant_feature_count": len(pairs),
        }

    set_info_sorted = sorted(info for (_eff, info) in tested)
    set_median = _median(set_info_sorted)
    # Degenerate set median (0): the abundance column cannot discriminate, so we
    # cannot form a meaningful ratio. Record the neutral 1.0 rather than divide
    # by zero or false-fail.
    if set_median is None or set_median == 0:
        return {
            "information_column_recorded": True,
            "top_effect_abundance_ratio": 1.0,
            "top_effect_k": int(top_k),
            "significant_feature_count": len(tested),
        }

    # The agent's own top-K by |effect| (descending). Stable order: |effect|
    # desc, then ascending info so ties are deterministic.
    ranked = sorted(tested, key=lambda p: (-p[0], p[1]))
    top = ranked[: int(top_k)]
    top_info_sorted = sorted(info for (_eff, info) in top)
    top_median = _median(top_info_sorted)
    ratio = top_median / set_median
    return {
        "information_column_recorded": True,
        "top_effect_abundance_ratio": float(ratio),
        "top_effect_k": int(top_k),
        "significant_feature_count": len(tested),
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
        # rather than fail-closing on a missing metric.
        write_result(
            {
                "information_column_recorded": False,
                "top_effect_abundance_ratio": 1.0,
                "top_effect_k": int(TOP_K),
                "significant_feature_count": 0,
            },
            args.out,
        )
        return 0
    effect_idx = _find_col(header, _EFFECT_COLS)
    if effect_idx is None:
        # No effect-size column at all -> nothing to rank; gate skips.
        write_result(
            {
                "information_column_recorded": False,
                "top_effect_abundance_ratio": 1.0,
                "top_effect_k": int(TOP_K),
                "significant_feature_count": 0,
            },
            args.out,
        )
        return 0
    info_idx = _find_col(header, _BASEMEAN_COLS)
    metrics = compute_metrics(rows, effect_idx, info_idx)
    write_result(metrics, args.out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
