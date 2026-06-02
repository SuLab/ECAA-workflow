# scripts/eval/tests/test_scorecard_fidelity.py
#
# Fidelity fixes:
#  - partial_judging rows (Gemini failed -> Opus-only fallback) must NOT pollute
#    the Gemini headline aggregates (_by_arm / paired delta / dimensions).
#  - the paired CI must carry a power guardrail: never "significant" at n==1, and
#    an `underpowered` flag below _MIN_POWER_PAIRS.
#  - the Nekrutenko handle-category histogram must render.
import json
from scripts.eval.benchmark import Score, Scorecard
from scripts.eval.services.scorecard import (
    _by_arm, paired_delta_summary, write_scorecard, _MIN_POWER_PAIRS,
)


def _row(arm, trial, overall, partial=False):
    extra = {"partial_judging": True} if partial else {}
    return Score("t1", arm, trial, overall, {}, None, None, "gemini-3.1-pro", extra=extra)


# ── partial_judging exclusion from the Gemini headline ───────────────────────

def test_by_arm_excludes_partial_judging_rows():
    rows = [_row("ecaa", 0, 80.0), _row("ecaa", 1, 20.0, partial=True)]
    by = _by_arm(Scorecard("b", rows))
    assert by["ecaa"] == [80.0]  # the Opus-only fallback row is not in the headline


def test_paired_delta_excludes_partial_judging_pairs():
    rows = [
        _row("ecaa", 0, 80.0), _row("claude-direct", 0, 70.0),
        _row("ecaa", 1, 90.0, partial=True), _row("claude-direct", 1, 10.0),
    ]
    s = paired_delta_summary(Scorecard("b", rows))
    assert s["n_pairs"] == 1  # trial-1 ecaa is partial-judging -> that pair drops


def test_partial_judging_caveat_rendered(tmp_path):
    rows = [_row("ecaa", 0, 80.0), _row("ecaa", 1, 20.0, partial=True),
            _row("claude-direct", 0, 70.0)]
    out = write_scorecard(Scorecard("biomnibench", rows), tmp_path)
    md = (out / "scorecard.md").read_text().lower()
    assert "partial-judging" in md or "partial judging" in md
    data = json.loads((out / "scorecard.json").read_text())
    assert data["meta"]["partial_judging_excluded"] == 1


# ── power guardrail on the paired CI ─────────────────────────────────────────

def test_single_pair_is_never_significant():
    rows = [_row("ecaa", 0, 80.0), _row("claude-direct", 0, 50.0)]
    s = paired_delta_summary(Scorecard("b", rows))
    assert s["n_pairs"] == 1
    assert s["significant"] is False     # a degenerate n==1 CI cannot be significant
    assert s["underpowered"] is True


def test_underpowered_flag_below_threshold():
    rows = []
    for t in range(_MIN_POWER_PAIRS - 1):
        rows += [_row("ecaa", t, 80.0), _row("claude-direct", t, 50.0)]
    s = paired_delta_summary(Scorecard("b", rows))
    assert s["underpowered"] is True


def test_not_underpowered_at_threshold():
    rows = []
    for t in range(_MIN_POWER_PAIRS + 2):
        rows += [_row("ecaa", t, 80.0), _row("claude-direct", t, 50.0)]
    s = paired_delta_summary(Scorecard("b", rows))
    assert s["underpowered"] is False


def test_underpowered_banner_in_markdown(tmp_path):
    rows = [_row("ecaa", 0, 80.0), _row("claude-direct", 0, 50.0)]
    md = (write_scorecard(Scorecard("biomnibench", rows), tmp_path) / "scorecard.md").read_text()
    assert "UNDERPOWERED" in md


# ── Nekrutenko handle-category histogram renders ─────────────────────────────

def test_handle_histogram_renders(tmp_path):
    card = Scorecard("nekrutenko", [
        Score("mtdna", "ecaa", 0, 100.0, {}, 1.0, None, "deterministic")],
        meta={"error_matrix": {"ecaa": {
            "recover_rate": 0.5, "diagnose_rate": 0.5, "n_cells": 4,
            "handle_counts": {"recover": 2, "partial": 1, "propagate": 0, "crash": 1},
            "by_pattern": {}}}})
    md = (write_scorecard(card, tmp_path) / "scorecard.md").read_text()
    assert "recover" in md and "crash" in md
    # The four-tuple signature is rendered for the arm.
    assert "2/1/0/1" in md or ("recover 2" in md and "crash 1" in md)


# ── F12 benchmarkable invariant set (readiness gate) ─────────────────────────

def _pkg_with_proofs(root, lines=('{"claim":"x","output":"y"}',)):
    """Materialize a minimal executed-ECAA package whose runtime/proofs.jsonl
    carries the given (non-blank) rows — the 04-C2 de-vacuifier the probe reads."""
    rt = root / "runtime"
    rt.mkdir(parents=True, exist_ok=True)
    (rt / "proofs.jsonl").write_text("\n".join(lines) + "\n")
    return root


def test_probe_devacuifiers_reads_proofs(tmp_path):
    from scripts.eval.services.scorecard import probe_devacuifiers
    # A non-empty proofs.jsonl flips evidence_from_proofs True (mirrors the Rust
    # `evidence_from_proofs` disk probe in benchmark_readiness.rs).
    p = probe_devacuifiers(_pkg_with_proofs(tmp_path))
    assert p["evidence_from_proofs"] is True
    assert p["signed_sink"] is False
    assert p["refs_projected"] is False


def test_probe_devacuifiers_blank_or_missing_proofs(tmp_path):
    from scripts.eval.services.scorecard import probe_devacuifiers
    # No package on disk at all -> all False.
    assert probe_devacuifiers(tmp_path / "nope")["evidence_from_proofs"] is False
    # A proofs.jsonl that is only blank lines/whitespace does NOT de-vacuify
    # (mirrors the Rust `!l.trim().is_empty()` row check).
    blank = _pkg_with_proofs(tmp_path / "blank", lines=("", "   ", "\t"))
    assert probe_devacuifiers(blank)["evidence_from_proofs"] is False
    # And a present signed sink flips signed_sink True.
    vr = tmp_path / "blank" / "runtime" / "verification-reports"
    vr.mkdir(parents=True, exist_ok=True)
    (vr / "claim-verification.signed.json").write_text("{}")
    assert probe_devacuifiers(blank)["signed_sink"] is True


def test_benchmarkable_set_with_proofs_includes_evidence_coverage(tmp_path):
    from scripts.eval.services.scorecard import (
        benchmarkable_set_meta, probe_devacuifiers,
    )
    # Probing a package with a non-empty proofs.jsonl makes the published set
    # 3-element — evidence_coverage joins the referential Inv 2/6 — matching the
    # Rust readiness gate's {decision_justification, substrate_validity,
    # evidence_coverage} on the corpus.
    m = benchmarkable_set_meta(**probe_devacuifiers(_pkg_with_proofs(tmp_path)))
    assert m["ready"] == [
        "decision_justification", "evidence_coverage", "substrate_validity",
    ]
    # Inv 1/5 (signed sink) and Inv 4 (refs) stay excluded with their reasons.
    assert set(m["excluded"]) == {
        "claim_completeness", "cross_graph_integrity", "equivalence_failure",
    }
    assert "Phase 1" in m["excluded"]["claim_completeness"]
    assert "04-C5" in m["excluded"]["equivalence_failure"]


def test_benchmarkable_set_without_proofs_stays_two_element(tmp_path):
    from scripts.eval.services.scorecard import (
        benchmarkable_set_meta, probe_devacuifiers,
    )
    # No proofs.jsonl -> evidence_coverage is still vacuous; the honest pre-Phase
    # fallback is the 2-element referential set.
    m = benchmarkable_set_meta(**probe_devacuifiers(tmp_path / "empty-pkg"))
    assert m["ready"] == ["decision_justification", "substrate_validity"]
    assert "04-C2" in m["excluded"]["evidence_coverage"]
    # The no-args default (no package context at all) keeps the same honest
    # all-false fallback.
    assert benchmarkable_set_meta()["ready"] == [
        "decision_justification", "substrate_validity",
    ]


def test_benchmarkable_set_meta_all_phases_done():
    from scripts.eval.services.scorecard import benchmarkable_set_meta
    m = benchmarkable_set_meta(signed_sink=True, refs_projected=True,
                               evidence_from_proofs=True)
    assert len(m["ready"]) == 6
    assert m["excluded"] == {}


def test_scorecard_injects_benchmarkable_set_with_proofs(tmp_path):
    # With a real package carrying proofs.jsonl, the injected set is 3-element
    # (includes evidence_coverage) — the published scorecard now agrees with the
    # Rust conformance suite instead of under-reporting.
    pkg = _pkg_with_proofs(tmp_path / "pkg")
    rows = [_row("ecaa", 0, 80.0), _row("claude-direct", 0, 70.0)]
    out = write_scorecard(Scorecard("biomnibench", rows), tmp_path / "out",
                          package_dir=pkg)
    data = json.loads((out / "scorecard.json").read_text())
    bs = data["meta"]["benchmarkable_set"]
    assert bs["ready"] == [
        "decision_justification", "evidence_coverage", "substrate_validity",
    ]
    md = (out / "scorecard.md").read_text()
    assert "Benchmarkable invariant set" in md
    assert "evidence_coverage" in md
    # An excluded invariant's reason is still rendered.
    assert "claim_completeness" in md


def test_scorecard_injects_benchmarkable_set_no_package(tmp_path):
    # No package context -> honest 2-element fallback (legacy call shape).
    rows = [_row("ecaa", 0, 80.0), _row("claude-direct", 0, 70.0)]
    out = write_scorecard(Scorecard("biomnibench", rows), tmp_path)
    data = json.loads((out / "scorecard.json").read_text())
    bs = data["meta"]["benchmarkable_set"]
    assert bs["ready"] == ["decision_justification", "substrate_validity"]
    md = (out / "scorecard.md").read_text()
    assert "Benchmarkable invariant set" in md
    assert "decision_justification" in md
