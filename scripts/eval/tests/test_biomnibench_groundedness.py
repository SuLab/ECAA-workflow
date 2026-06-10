"""Claim-groundedness metric for the BiomniBench plugin (offline).

The extractor is an admitted HEURISTIC: it splits the narrative into sentences,
keeps those carrying a quantitative/directional/comparative claim marker, and
counts a claim "grounded" when its salient token (a number, gene/identifier, or
PMID) re-appears in the flattened result rows. This is noisy (false +/- both
possible); the metric is a VISIBILITY signal, never a gate. These tests pin the
heuristic's behaviour and confirm both the batch (assemble_score) and synchronous
(score) paths stash the shared Score.extra["claim_groundedness"] shape.
"""
from scripts.eval.benchmark import Arm, Output, Task
from scripts.eval.plugins.biomnibench import (
    BiomniBench,
    _extract_claims,
    _grounding_reference_type,
    compute_claim_groundedness,
)


# ── _extract_claims ──────────────────────────────────────────────────────────

def test_extract_claims_keeps_only_claim_bearing_sentences():
    narrative = (
        "We loaded the data. "
        "Expression of TP53 increased 2.4-fold in the treated group. "
        "This is a transition sentence. "
        "BRCA1 was significantly downregulated (p=0.003)."
    )
    claims = _extract_claims(narrative)
    # The bare "We loaded the data" / "transition sentence" lines carry no
    # quantitative/directional marker and are dropped.
    assert any("TP53" in c for c in claims)
    assert any("BRCA1" in c for c in claims)
    assert not any("transition sentence" in c for c in claims)
    assert len(claims) == 2


def test_extract_claims_empty_narrative_is_empty():
    assert _extract_claims("") == []
    assert _extract_claims("   \n  ") == []


# ── _grounding_reference_type ────────────────────────────────────────────────

def test_grounding_reference_type_classifies_source():
    assert _grounding_reference_type(has_row=True, has_pmid=False) == "result_row"
    assert _grounding_reference_type(has_row=False, has_pmid=True) == "pmid"
    assert _grounding_reference_type(has_row=True, has_pmid=True) == "mixed"
    # No evidence at all defaults to result_row (the primary reference surface).
    assert _grounding_reference_type(has_row=False, has_pmid=False) == "result_row"


# ── compute_claim_groundedness ───────────────────────────────────────────────

def test_compute_claim_groundedness_matches_result_rows():
    narrative = (
        "Expression of TP53 increased 2.4-fold in the treated cohort. "
        "GENEX showed a 99-fold change that appears nowhere downstream."
    )
    # Result text mentions TP53 and the 2.4 magnitude; GENEX/99 absent.
    result_text = "gene\tlog2fc\nTP53\t2.4\nMYC\t1.1\n"
    out = compute_claim_groundedness(narrative, result_text)
    assert out["total_claims"] == 2
    assert out["verified_count"] == 1
    assert out["verified_pct"] == 50.0
    assert out["reference_type"] == "result_row"


def test_compute_claim_groundedness_empty_when_no_claims():
    out = compute_claim_groundedness("We ran the pipeline.", "anything")
    assert out["total_claims"] == 0
    assert out["verified_count"] == 0
    assert out["verified_pct"] == 0.0
    assert out["reference_type"] == "result_row"


def test_compute_claim_groundedness_pmid_reference():
    narrative = "TP53 was upregulated 2.0-fold, consistent with PMID 12345678."
    # No tabular result text; grounding lands on the PMID echoed in result rows.
    result_text = "evidence\nPMID:12345678 TP53 oncogene\n"
    out = compute_claim_groundedness(narrative, result_text)
    assert out["total_claims"] == 1
    assert out["verified_count"] == 1
    assert out["reference_type"] in ("pmid", "mixed")


def test_compute_claim_groundedness_shape_keys():
    # The contract shape must always carry exactly these four keys.
    out = compute_claim_groundedness("TP53 increased 2.0-fold.", "TP53\t2.0\n")
    assert set(out) == {"verified_count", "total_claims", "verified_pct",
                        "reference_type"}
    assert isinstance(out["verified_count"], int)
    assert isinstance(out["total_claims"], int)
    assert isinstance(out["verified_pct"], float)
    assert out["reference_type"] in ("result_row", "pmid", "mixed")


# ── Score.extra population (batch + sync) ────────────────────────────────────

_RUBRIC = {"criteria": [
    {"id": "c1", "dimension": "result_correctness", "points": 4,
     "levels": {"A": 1.0, "B": 0.5, "C": 0.0}}]}


def _gnd_output():
    return Output(
        trace_md="TP53 increased 2.4-fold in the treated cohort.",
        answer_txt="gene\tlog2fc\nTP53\t2.4\n",
        artifacts={}, exit_ok=True, wall_secs=1.0)


def test_assemble_score_populates_claim_groundedness():
    task = Task(task_id="t1", prompt="q", inputs={}, rubric=_RUBRIC, answer_key=None)
    verdicts = {
        "headline": {"overall": 80.0, "dimensions": {"result_correctness": 80.0},
                     "levels": {"c1": "A"}, "cost_usd": 0.0},
        "cross": {"overall": 78.0, "dimensions": {"result_correctness": 78.0},
                  "levels": {"c1": "A"}, "cost_usd": 0.0},
    }
    s = BiomniBench().assemble_score(task, Arm.ECAA_WORKFLOW, _gnd_output(), 0, verdicts)
    cg = s.extra["intra_narrative_self_consistency"]
    assert cg["total_claims"] == 1
    assert cg["verified_count"] == 1
    assert cg["verified_pct"] == 100.0
    assert cg["reference_type"] == "result_row"


def test_assemble_score_groundedness_present_even_when_partial_judging():
    task = Task(task_id="t1", prompt="q", inputs={}, rubric=_RUBRIC, answer_key=None)
    # Only the cross judge present -> partial_judging, but groundedness is
    # judge-independent and must still be computed.
    verdicts = {"cross": {"overall": 60.0, "dimensions": {}, "levels": {}, "cost_usd": 0.0}}
    s = BiomniBench().assemble_score(task, Arm.ECAA_WORKFLOW, _gnd_output(), 0, verdicts)
    assert s.extra["partial_judging"] is True
    assert s.extra["intra_narrative_self_consistency"]["total_claims"] == 1


def test_score_sync_path_populates_claim_groundedness(monkeypatch):
    # The synchronous score() path calls the live judge twice; stub it so the
    # test stays offline and isolates the groundedness computation.
    def _fake_judge(judge_id, rubric, trace, answer):
        return {"overall": 70.0, "dimensions": {"result_correctness": 70.0},
                "levels": {"c1": "A"}, "cost_usd": 0.0}

    monkeypatch.setattr("scripts.eval.plugins.biomnibench.judge", _fake_judge)
    task = Task(task_id="t1", prompt="q", inputs={}, rubric=_RUBRIC, answer_key=None)
    s = BiomniBench().score(task, Arm.ECAA_WORKFLOW, _gnd_output(), 0)
    cg = s.extra["intra_narrative_self_consistency"]
    assert cg["total_claims"] == 1
    assert cg["verified_count"] == 1
    assert cg["verified_pct"] == 100.0
    assert cg["reference_type"] == "result_row"
