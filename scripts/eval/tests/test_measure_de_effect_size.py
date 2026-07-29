"""Unit tests for the byte-pinned DE effect-size-reliability measurement script.

The script is shipped into emitted packages' lib/ and run verbatim in the
bio-min container by a differential_expression task. It recomputes a single
method-neutral domain-correctness scalar from the agent's OWN results table: the
typical (median) abundance of the agent's own top-by-|effect| features AS A
RATIO to the typical abundance over the whole tested set. The ratio is
null-robust — ≈1 under independence (effect ⟂ abundance), driven toward 0 by an
extreme effect concentrated in the lowest-abundance features. These tests
exercise that core on hand-built tables (a planted low-count artifact vs a clean
table vs a table with no abundance column vs a degenerate column) and the
end-to-end TSV path.
"""
import importlib.util
import json
import re
from pathlib import Path

_SCRIPT = Path(__file__).resolve().parents[2] / "measure_de_effect_size.py"


def _load_module():
    spec = importlib.util.spec_from_file_location("measure_de_effect_size", _SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_reference_bounds_are_operator_authored_constants():
    mod = _load_module()
    # Operator-authored, SME-overridable by editing the pinned script; never
    # handed to the agent (same precedent as NOISE_FLOOR/HOMOPLASMY_CUTOFF).
    assert mod.TOP_K == 15
    assert mod.MIN_ABUNDANCE_RATIO == 0.20


def _planted_artifact_rows():
    """The da-15-1 signature: a well-powered modest-effect bulk (40 features at
    log2fc ~1, base_mean 20+), plus a handful of near-zero-abundance features
    carrying the most-extreme |log2fc| (HAND2 -14.66 @ base_mean 1.59,
    LGALS9B -13.68 @ base_mean 0.61). The agent's top-15-by-|effect| are these
    low-abundance hits, whose median abundance is a small fraction of the tested
    set's median -> ratio well below 0.20."""
    rows = [[f"good{i}", f"{1.0 + 0.02 * i}", f"{20 + i * 5}"] for i in range(40)]
    rows += [[f"art{i}", f"{-14.7 + i * 0.05}", f"{0.4 + i * 0.1}"] for i in range(20)]
    return rows


def test_planted_low_count_artifacts_flagged():
    mod = _load_module()
    header = ["feature", "log2fc", "base_mean"]
    eff = mod._find_col(header, mod._EFFECT_COLS)
    info = mod._find_col(header, mod._BASEMEAN_COLS)
    res = mod.compute_metrics(_planted_artifact_rows(), eff, info)
    assert res["information_column_recorded"] is True
    # The strongest-by-effect features sit at a small fraction of the tested
    # set's typical abundance -> ratio below the operator bound 0.20 (FAILS,
    # correctly — the da-15-1 signature).
    assert res["top_effect_abundance_ratio"] < mod.MIN_ABUNDANCE_RATIO, res


def test_clean_table_when_strong_hits_are_high_abundance():
    mod = _load_module()
    header = ["feature", "log2fc", "base_mean"]
    eff = mod._find_col(header, mod._EFFECT_COLS)
    info = mod._find_col(header, mod._BASEMEAN_COLS)
    # Effect size increases with abundance: the strongest hits are well-powered,
    # so the top-K abundance median is at or above the set median -> ratio >= 1.
    clean = [[f"g{i}", f"{0.5 + i * 0.4}", f"{10 + i * 10}"] for i in range(40)]
    res = mod.compute_metrics(clean, eff, info)
    assert res["information_column_recorded"] is True
    assert res["top_effect_abundance_ratio"] >= mod.MIN_ABUNDANCE_RATIO, res


def test_no_abundance_column_skips_via_information_column_recorded_false():
    mod = _load_module()
    # A Wilcoxon/MAST-style table with no base-mean-equivalent column: the
    # measure records information_column_recorded=false so the contract's
    # `when` gate SKIPS the check (never false-fails, never prescribes adding a
    # column). The ratio is recorded as the neutral, non-failing sentinel 1.0.
    header = ["feature", "log2fc", "padj"]
    eff = mod._find_col(header, mod._EFFECT_COLS)
    info = mod._find_col(header, mod._BASEMEAN_COLS)
    assert info is None
    res = mod.compute_metrics(
        [[f"g{i}", "5.0", "0.01"] for i in range(10)], eff, info
    )
    assert res["information_column_recorded"] is False, res
    assert res["top_effect_abundance_ratio"] == 1.0, res


def test_column_convention_robustness():
    mod = _load_module()
    rows = _planted_artifact_rows()
    base = None
    # The metric keys on semantic ROLE, so DESeq2/edgeR/limma-voom header
    # conventions all produce an identical verdict.
    for header in (
        ["feature", "log2fc", "base_mean"],
        ["gene", "log2FoldChange", "baseMean"],
        ["protein", "logFC", "AveExpr"],
        ["id", "lfc", "logCPM"],
    ):
        eff = mod._find_col(header, mod._EFFECT_COLS)
        info = mod._find_col(header, mod._BASEMEAN_COLS)
        res = mod.compute_metrics(rows, eff, info)
        if base is None:
            base = res
        assert res == base, (header, res, base)
    assert base["top_effect_abundance_ratio"] < mod.MIN_ABUNDANCE_RATIO


def test_empty_abundance_values_read_as_not_recorded():
    mod = _load_module()
    # Column present in the header but every value unparseable (all-blank /
    # all-NaN) -> not recorded -> the gate skips rather than dividing on an
    # empty set. The ratio is the neutral sentinel 1.0.
    header = ["feature", "log2fc", "base_mean"]
    eff = mod._find_col(header, mod._EFFECT_COLS)
    info = mod._find_col(header, mod._BASEMEAN_COLS)
    res = mod.compute_metrics(
        [[f"g{i}", "5.0", "NA"] for i in range(10)], eff, info
    )
    assert res["information_column_recorded"] is False, res
    assert res["top_effect_abundance_ratio"] == 1.0, res


def test_degenerate_zero_abundance_does_not_false_fail():
    mod = _load_module()
    # The abundance column is present and numeric but the set median is 0
    # (degenerate — cannot discriminate). The script must record the neutral
    # ratio 1.0 rather than divide by zero or false-fail.
    header = ["feature", "log2fc", "base_mean"]
    eff = mod._find_col(header, mod._EFFECT_COLS)
    info = mod._find_col(header, mod._BASEMEAN_COLS)
    res = mod.compute_metrics(
        [[f"g{i}", f"{i}", "0"] for i in range(20)], eff, info
    )
    assert res["information_column_recorded"] is True, res
    assert res["top_effect_abundance_ratio"] == 1.0, res


def test_end_to_end_tsv_and_sorted_keys(tmp_path):
    mod = _load_module()
    table = tmp_path / "de_results.tsv"
    header = ["feature", "log2fc", "base_mean"]
    lines = ["\t".join(header)]
    for row in _planted_artifact_rows():
        lines.append("\t".join(row))
    table.write_text("\n".join(lines) + "\n")
    out = tmp_path / "result.json"
    rc = mod.main(["--table", str(table), "--out", str(out)])
    assert rc == 0
    parsed = json.loads(out.read_text())
    assert parsed["information_column_recorded"] is True
    assert parsed["top_effect_abundance_ratio"] < mod.MIN_ABUNDANCE_RATIO
    # Deterministic on-disk shape.
    assert list(parsed.keys()) == sorted(parsed.keys()), "result.json keys must be sorted"


def test_emitted_count_key_is_tested_feature_count_not_significant(tmp_path):
    """Twin for the key rename: the count is over TESTED features, so the
    emitted JSON key must be `tested_feature_count`. The misleading
    `significant_feature_count` (it never counted significance) must be gone,
    and the value must equal the number of tested features."""
    mod = _load_module()
    table = tmp_path / "de_results.tsv"
    rows = _planted_artifact_rows()
    lines = ["\t".join(["feature", "log2fc", "base_mean"])]
    for row in rows:
        lines.append("\t".join(row))
    table.write_text("\n".join(lines) + "\n")
    out = tmp_path / "result.json"
    assert mod.main(["--table", str(table), "--out", str(out)]) == 0
    parsed = json.loads(out.read_text())
    assert "tested_feature_count" in parsed, parsed
    assert "significant_feature_count" not in parsed, parsed
    # Every planted row carries a usable effect AND abundance, so all are tested.
    assert parsed["tested_feature_count"] == len(rows)

    # The pure compute path emits the same key.
    eff, info = 1, 2
    res = mod.compute_metrics(rows, eff, info)
    assert "tested_feature_count" in res
    assert "significant_feature_count" not in res


def test_end_to_end_missing_table_emits_skip_result(tmp_path):
    mod = _load_module()
    out = tmp_path / "result.json"
    rc = mod.main(["--table", str(tmp_path / "does-not-exist.tsv"), "--out", str(out)])
    assert rc == 0
    parsed = json.loads(out.read_text())
    # A missing/unreadable table emits a not-recorded result so the gate skips
    # rather than fail-closing on a missing metric.
    assert parsed["information_column_recorded"] is False
    assert parsed["top_effect_abundance_ratio"] == 1.0
    # The report-completeness flags are also false (no header) and the gate skips.
    assert parsed["r_squared_column_recorded"] is False
    assert parsed["sample_size_column_recorded"] is False


# ---------------------------------------------------------------------------
# Report-completeness (da-8-1 C8): model-fit + per-row-n presence flags + the
# folded narrative_text channel. The flags mirror information_column_recorded
# EXACTLY (FOUND-and-usable), so the contract's `when` gate skips when the column
# is absent and never prescribes producing it.
# ---------------------------------------------------------------------------


def test_modelfit_and_samplesize_role_detection():
    mod = _load_module()
    # Header-convention role detection across common R-squared / n synonyms,
    # case-insensitive, exactly like _EFFECT_COLS / _BASEMEAN_COLS — names no
    # method.
    for h in ("r_squared", "R2", "adj_r_squared", "pseudo_r2"):
        assert mod._find_col(["g", h], mod._MODELFIT_COLS) == 1, h
    for h in ("n", "sample_size", "N_obs", "num_samples"):
        assert mod._find_col(["g", h], mod._SAMPLESIZE_COLS) == 1, h


def test_presence_flags_true_when_column_recorded():
    mod = _load_module()
    header = ["gene", "log2fc", "base_mean", "r_squared", "n"]
    rows = [["g1", "1.0", "20", "0.85", "12"], ["g2", "2.0", "30", "0.6", "11"]]
    flags = mod.report_completeness_flags(header, rows)
    assert flags["r_squared_column_recorded"] is True, flags
    assert flags["sample_size_column_recorded"] is True, flags


def test_presence_flags_false_when_column_absent_so_gate_skips():
    mod = _load_module()
    # No model-fit / sample-size column -> flags false -> contract `when` SKIPS
    # (never blocks, never prescribes adding the column).
    header = ["gene", "log2fc", "base_mean"]
    flags = mod.report_completeness_flags(header, [["g1", "1.0", "20"]])
    assert flags["r_squared_column_recorded"] is False, flags
    assert flags["sample_size_column_recorded"] is False, flags


def test_presence_flags_false_when_column_header_only_or_all_nan():
    mod = _load_module()
    # Column present in the header but every value unparseable -> not recorded
    # (same FOUND-and-usable semantics as information_column_recorded, which uses
    # plain float(): "NA"/""/"n/a" are unparseable, so the flag reads False and
    # the contract `when` gate skips).
    header = ["gene", "r2", "sample_size"]
    flags = mod.report_completeness_flags(header, [["g1", "NA", ""], ["g2", "", "n/a"]])
    assert flags["r_squared_column_recorded"] is False, flags
    assert flags["sample_size_column_recorded"] is False, flags


def test_fold_narrative_text_folds_siblings_by_precedence_and_own_narrative(tmp_path):
    mod = _load_module()
    out = tmp_path / "result.json"
    # Pre-existing result.json carrying the agent's own narrative field.
    out.write_text(json.dumps({"narrative": "OWN model fit R-squared = 0.85"}))
    # Sibling deliverables (report > interpretation > summary > other precedence).
    (tmp_path / "report.md").write_text("REPORT variance explained = 0.85.")
    (tmp_path / "answer.txt").write_text("ANSWER sample size n = 41 per metabolite.")
    folded = mod._fold_narrative_text(str(out))
    # Both channels (own + sibling) fold into one searchable blob.
    assert "OWN model fit" in folded
    assert "REPORT variance explained" in folded
    assert "ANSWER sample size" in folded
    # result.json itself is never re-read as a narrative sibling.
    assert folded.count("OWN model fit") == 1
    # Precedence: report.md (precedence 0) sorts before answer.txt (precedence 3).
    assert folded.index("REPORT variance explained") < folded.index("ANSWER sample")


def test_fold_narrative_text_empty_when_no_artifacts(tmp_path):
    mod = _load_module()
    out = tmp_path / "result.json"
    # No siblings, no pre-existing result.json -> empty string (the contract's
    # substring search then matches nothing, but the `when` gate already skips
    # when no column was recorded, so an empty narrative never false-fails).
    assert mod._fold_narrative_text(str(out)) == ""


def test_end_to_end_emits_completeness_flags_and_narrative_text(tmp_path):
    mod = _load_module()
    header = ["feature", "log2fc", "base_mean", "r_squared", "n"]
    table = tmp_path / "de_results.tsv"
    lines = ["\t".join(header)]
    for i in range(20):
        lines.append("\t".join([f"g{i}", f"{1.0 + i}", f"{10 + i}", "0.8", "12"]))
    table.write_text("\n".join(lines) + "\n")
    out = tmp_path / "result.json"
    # A sibling narrative that DOES surface both statistics.
    (tmp_path / "report.md").write_text(
        "The model R-squared is 0.8 and the sample size n = 12 per feature."
    )
    rc = mod.main(["--table", str(table), "--out", str(out)])
    assert rc == 0
    parsed = json.loads(out.read_text())
    assert parsed["r_squared_column_recorded"] is True, parsed
    assert parsed["sample_size_column_recorded"] is True, parsed
    assert "R-squared is 0.8" in parsed["narrative_text"]
    assert "n = 12" in parsed["narrative_text"]
    # Deterministic on-disk shape preserved with the new keys.
    assert list(parsed.keys()) == sorted(parsed.keys())


def test_end_to_end_no_effect_column_still_records_completeness_flags(tmp_path):
    mod = _load_module()
    # A table with no effect column (the abundance-ratio gate skips) can still
    # record an R-squared / n column; the completeness flags must still be set.
    header = ["feature", "r_squared", "n"]
    table = tmp_path / "fit.tsv"
    table.write_text("\t".join(header) + "\n" + "x\t0.7\t9\ny\t0.6\t8\n")
    out = tmp_path / "result.json"
    rc = mod.main(["--table", str(table), "--out", str(out)])
    assert rc == 0
    parsed = json.loads(out.read_text())
    assert parsed["information_column_recorded"] is False  # no effect col to rank
    assert parsed["r_squared_column_recorded"] is True, parsed
    assert parsed["sample_size_column_recorded"] is True, parsed


# ---------------------------------------------------------------------------
# Self-describing metric: the ratio is emitted WITH the numerator/denominator
# populations, their sizes, K, and the count of top-K features carrying no usable
# significance value. A bare number let a deposited report invent a denominator
# ("all 4,030 significant genes") for a whole-tested-set ratio, and let a sibling
# report name BOTH populations inside one sentence.
# ---------------------------------------------------------------------------

#: Every key the basis block carries, in EVERY branch. A stable key set means a
#: downstream reader never branches on presence.
_BASIS_KEYS = {
    "computed",
    "denominator",
    "denominator_population",
    "denominator_population_size",
    "denominator_statistic",
    "effect_column",
    "information_column",
    "not_computed_reason",
    "numerator",
    "numerator_population",
    "numerator_population_size",
    "numerator_statistic",
    "requested_top_k",
    "significance_column",
    "statistic",
    "top_k_missing_significance_count",
    "value",
}


def _sig_table(tmp_path, name="de_results.tsv", *, missing_significance=0):
    """A table whose top-15-by-|effect| are the LOWEST-abundance features (so the
    ratio is computed), with `missing_significance` of those top features
    carrying an unusable significance cell."""
    header = ["gene", "log2FoldChange", "baseMean", "padj"]
    rows = []
    # 40 modest-effect, well-supported features: the tested-set bulk.
    for i in range(40):
        rows.append([f"bulk{i}", f"{1.0 + 0.02 * i}", f"{100 + i}", "0.2"])
    # 20 extreme-effect, near-absent features: the top-15 come from here.
    for i in range(20):
        padj = "NA" if i < missing_significance else "0.001"
        rows.append([f"tail{i}", f"{-14.7 + i * 0.05}", f"{0.5 + i * 0.1}", padj])
    table = tmp_path / name
    table.write_text(
        "\n".join(["\t".join(header)] + ["\t".join(r) for r in rows]) + "\n"
    )
    return table


def test_metric_block_is_present_and_self_consistent(tmp_path):
    mod = _load_module()
    out = tmp_path / "result.json"
    assert mod.main(["--table", str(_sig_table(tmp_path)), "--out", str(out)]) == 0
    parsed = json.loads(out.read_text())
    basis = parsed["top_effect_abundance_ratio_basis"]
    # The block never disagrees with the bare number the contract reads.
    assert basis["value"] == parsed["top_effect_abundance_ratio"]
    assert basis["computed"] is True
    assert basis["not_computed_reason"] is None
    assert basis["requested_top_k"] == parsed["top_effect_k"]
    # The DENOMINATOR population is the whole tested set — the exact fact a
    # deposited report got wrong by naming the significant subset instead.
    assert basis["denominator_population"] == mod.DENOMINATOR_POPULATION_ID
    assert basis["denominator_population_size"] == parsed["tested_feature_count"]
    assert basis["numerator_population"] == mod.NUMERATOR_POPULATION_ID
    assert basis["numerator_population_size"] == mod.TOP_K
    assert basis["numerator_statistic"] == basis["denominator_statistic"] == "median"
    assert basis["statistic"] == mod.RATIO_STATISTIC_ID
    # The run's OWN column names, so the description is never generic when the
    # table names them.
    assert basis["effect_column"] == "log2FoldChange"
    assert basis["information_column"] == "baseMean"
    assert basis["significance_column"] == "padj"
    assert set(basis) == _BASIS_KEYS, set(basis) ^ _BASIS_KEYS


def test_description_states_both_population_sizes_and_rules_out_substitutes(tmp_path):
    mod = _load_module()
    out = tmp_path / "result.json"
    assert mod.main(["--table", str(_sig_table(tmp_path)), "--out", str(out)]) == 0
    parsed = json.loads(out.read_text())
    text = parsed["top_effect_abundance_ratio_description"]
    basis = parsed["top_effect_abundance_ratio_basis"]
    # Both population sizes are IN the sentence, so quoting it verbatim cannot
    # lose the denominator.
    assert str(basis["numerator_population_size"]) in text
    assert str(basis["denominator_population_size"]) in text
    # The value as a report renders it (the anchor the Rust prose check looks for).
    assert f"{basis['value']:.4f}" in text
    # And it names the columns it was computed over.
    assert "`baseMean`" in text and "`log2FoldChange`" in text
    # The substitutions real reports made are ruled out explicitly.
    lower = text.lower()
    assert "not a significant subset" in lower
    assert "both terms are medians" in lower
    assert "not a per-sample statistic" in lower
    # The description is meant to be QUOTED verbatim into a report, and the
    # in-tree prose check warns when "mean"/"average" appears near a ratio
    # citation — so the description must not contain either word itself.
    # `baseMean`/`base_mean`-style column names are word-internal and exempt.
    assert not re.search(r"\b(?:mean|means|average|averaged|averages)\b", lower), text


def test_description_is_deterministic_across_runs(tmp_path):
    mod = _load_module()
    table = _sig_table(tmp_path)
    first, second = tmp_path / "a.json", tmp_path / "b.json"
    assert mod.main(["--table", str(table), "--out", str(first)]) == 0
    assert mod.main(["--table", str(table), "--out", str(second)]) == 0
    a, b = json.loads(first.read_text()), json.loads(second.read_text())
    for key in ("top_effect_abundance_ratio_basis", "top_effect_abundance_ratio_description"):
        assert a[key] == b[key], key
    # Byte-identical on disk too (sorted keys, no clock, no ordering nondeterminism).
    assert first.read_text() == second.read_text()


def test_top_k_missing_significance_count_is_over_the_top_set(tmp_path):
    mod = _load_module()
    # 4 of the tail features carry `padj = NA`, and the tail supplies the whole
    # top-15 — the deposited run's exact shape (4 of the top 15 had padj = NA, so
    # the top set was NOT a subset of the significant set the prose named).
    for missing in (0, 1, 4, 15):
        out = tmp_path / f"result_{missing}.json"
        table = _sig_table(tmp_path, f"de_{missing}.tsv", missing_significance=missing)
        assert mod.main(["--table", str(table), "--out", str(out)]) == 0
        parsed = json.loads(out.read_text())
        basis = parsed["top_effect_abundance_ratio_basis"]
        assert basis["top_k_missing_significance_count"] == missing, (missing, basis)
        text = parsed["top_effect_abundance_ratio_description"]
        assert f"{missing} of those 15 top features carry no usable `padj`" in text
        # The "not a subset of the significant set" clause is a consequence of a
        # NONZERO missing count, so it appears exactly when it is true.
        assert ("NOT a subset of the significant set" in text) is (missing > 0), text


def test_missing_significance_count_is_null_when_no_significance_column(tmp_path):
    mod = _load_module()
    # No significance column at all: 0 would read as "every top feature has one",
    # a different and unsupported claim, so the count is null and the description
    # says the metric supports no statement about a significant subset.
    header = ["feature", "log2fc", "base_mean"]
    table = tmp_path / "nosig.tsv"
    lines = ["\t".join(header)]
    for row in _planted_artifact_rows():
        lines.append("\t".join(row))
    table.write_text("\n".join(lines) + "\n")
    out = tmp_path / "result.json"
    assert mod.main(["--table", str(table), "--out", str(out)]) == 0
    parsed = json.loads(out.read_text())
    basis = parsed["top_effect_abundance_ratio_basis"]
    assert basis["top_k_missing_significance_count"] is None, basis
    assert basis["significance_column"] is None, basis
    assert "no statement about a significant subset" in (
        parsed["top_effect_abundance_ratio_description"]
    )


def test_significance_column_does_not_change_the_ratio(tmp_path):
    mod = _load_module()
    # The significance role is COUNTED, never used to filter the tested set or
    # rank the top-K: the same table with and without a padj column must yield
    # the identical ratio (the deposited value's denominator is the tested set,
    # not the significant set).
    rows = _planted_artifact_rows()
    eff, info = 1, 2
    without = mod.compute_metrics(rows, eff, info)
    with_sig = mod.compute_metrics(
        [r + ["NA"] for r in rows], eff, info, sig_idx=3
    )
    assert without["top_effect_abundance_ratio"] == with_sig["top_effect_abundance_ratio"]
    assert without["tested_feature_count"] == with_sig["tested_feature_count"]
    # Only the new count differs: every top feature lacks a usable padj.
    assert with_sig["top_effect_abundance_ratio_basis"]["top_k_missing_significance_count"] == 15
    assert without["top_effect_abundance_ratio_basis"]["top_k_missing_significance_count"] is None


def test_not_computed_paths_say_so_instead_of_naming_a_population(tmp_path):
    mod = _load_module()
    # Every gate-skip / degenerate branch must emit the block with computed=false,
    # a null value, and a description that refuses to describe a population —
    # otherwise the 1.0 sentinel becomes quotable as a measurement.
    header = ["feature", "log2fc", "padj"]
    cases = [
        # (result, expected reason substring)
        (
            mod.compute_metrics([[f"g{i}", "5.0", "0.01"] for i in range(10)], 1, None),
            "abundance/information column",
        ),
        (
            mod.compute_metrics([[f"g{i}", f"{i}", "0"] for i in range(20)], 1, 2),
            "median abundance over the tested set is 0",
        ),
    ]
    assert mod._find_col(header, mod._BASEMEAN_COLS) is None  # case 1 precondition
    for result, reason in cases:
        basis = result["top_effect_abundance_ratio_basis"]
        assert basis["computed"] is False, basis
        assert basis["value"] is None, basis
        assert reason in basis["not_computed_reason"], basis
        assert set(basis) == _BASIS_KEYS, set(basis) ^ _BASIS_KEYS
        text = result["top_effect_abundance_ratio_description"]
        assert "was NOT computed" in text, text
        assert "non-failing sentinel, not a measurement" in text, text
        assert "do not describe a population for it" in text, text
        # The sentinel is still recorded for the `when`-gated contract.
        assert result["top_effect_abundance_ratio"] == 1.0


def test_missing_table_and_no_effect_column_also_carry_the_block(tmp_path):
    mod = _load_module()
    out = tmp_path / "missing.json"
    assert mod.main(["--table", str(tmp_path / "nope.tsv"), "--out", str(out)]) == 0
    basis = json.loads(out.read_text())["top_effect_abundance_ratio_basis"]
    assert basis["computed"] is False and basis["value"] is None
    assert "no readable results table" in basis["not_computed_reason"]
    assert set(basis) == _BASIS_KEYS

    fit = tmp_path / "fit.tsv"
    fit.write_text("feature\tr_squared\tn\nx\t0.7\t9\n")
    out2 = tmp_path / "fit.json"
    assert mod.main(["--table", str(fit), "--out", str(out2)]) == 0
    basis2 = json.loads(out2.read_text())["top_effect_abundance_ratio_basis"]
    assert basis2["computed"] is False and basis2["value"] is None
    assert "no per-feature effect-size column" in basis2["not_computed_reason"]
    assert set(basis2) == _BASIS_KEYS


def test_description_falls_back_to_role_words_when_columns_are_unnamed():
    mod = _load_module()
    # compute_metrics called without column_names (the pure-core path a caller
    # may use) must still describe the populations — with generic ROLE words, so
    # the description is never silently modality-specific.
    res = mod.compute_metrics(_planted_artifact_rows(), 1, 2)
    text = res["top_effect_abundance_ratio_description"]
    assert mod._GENERIC_INFORMATION_LABEL in text, text
    assert mod._GENERIC_EFFECT_LABEL in text, text
    assert "`" not in text, "no column was named, so nothing may be backticked"


def test_pre_existing_keys_are_unchanged_by_the_additions():
    mod = _load_module()
    # Additive only: the keys existing readers bind to keep their exact names,
    # types, and values.
    res = mod.compute_metrics(_planted_artifact_rows(), 1, 2)
    assert res["information_column_recorded"] is True
    assert res["top_effect_k"] == mod.TOP_K
    assert res["tested_feature_count"] == len(_planted_artifact_rows())
    assert isinstance(res["top_effect_abundance_ratio"], float)
    assert res["top_effect_abundance_ratio"] < mod.MIN_ABUNDANCE_RATIO
