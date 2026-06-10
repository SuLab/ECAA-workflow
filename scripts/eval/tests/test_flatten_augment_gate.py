import json
from pathlib import Path

from scripts.eval.scoring.flatten import flatten_outputs


def _stage(tmp_path: Path, claims: list[dict]) -> tuple[Path, Path]:
    """Build a minimal one-task package: WORKFLOW.json + outputs/<tid>/."""
    tid = "report_final"
    wf = tmp_path / "WORKFLOW.json"
    wf.write_text(json.dumps({"tasks": {tid: {"id": tid, "depends_on": []}}}))
    out = tmp_path / "outputs" / tid
    out.mkdir(parents=True)
    (out / "report.md").write_text("We found 89408 baseline tumour cells.\n")
    (out / "result.json").write_text(json.dumps({
        "claims": claims}))
    return out.parent, wf


def test_claims_block_omitted_by_default(tmp_path, monkeypatch):
    monkeypatch.delenv("ECAA_EVAL_NARRATIVE_AUGMENT", raising=False)
    outputs_dir, wf = _stage(
        tmp_path, [{"claim": "TP53 up 2.0-fold", "evidence": "table1.csv"}])
    trace, answer = flatten_outputs(outputs_dir, wf)
    assert "Structured claims" not in trace
    assert "Structured claims" not in answer
    assert "table1.csv" not in trace
    # The raw narrative still flows through.
    assert "89408 baseline tumour cells" in answer


def test_claims_block_present_when_augment_opted_in(tmp_path, monkeypatch):
    monkeypatch.setenv("ECAA_EVAL_NARRATIVE_AUGMENT", "1")
    outputs_dir, wf = _stage(
        tmp_path, [{"claim": "TP53 up 2.0-fold", "evidence": "table1.csv"}])
    trace, answer = flatten_outputs(outputs_dir, wf)
    assert "Structured claims" in trace
    assert "table1.csv" in answer


def test_augment_explicit_zero_is_off(tmp_path, monkeypatch):
    monkeypatch.setenv("ECAA_EVAL_NARRATIVE_AUGMENT", "0")
    outputs_dir, wf = _stage(
        tmp_path, [{"claim": "TP53 up 2.0-fold", "evidence": "table1.csv"}])
    trace, answer = flatten_outputs(outputs_dir, wf)
    assert "Structured claims" not in trace
    assert "Structured claims" not in answer
