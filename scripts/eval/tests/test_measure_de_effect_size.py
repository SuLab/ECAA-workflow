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
