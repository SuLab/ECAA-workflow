"""The unattended eval must pre-approve discover_* selections so the workflow
advances instead of stalling on AwaitingSmeApproval (no SME in a benchmark)."""
import json
from scripts.eval import eval_runner


def test_write_auto_approve_discoveries(tmp_path):
    pkg = tmp_path / "pkg"
    pkg.mkdir()
    (pkg / "WORKFLOW.json").write_text(json.dumps({"tasks": {
        "discover_alignment": {"spec": {"stage_class": "alignment"}},
        "discover_variant_calling": {"spec": {"stage_class": "variant_calling"}},
        "discover_no_spec": {},                 # falls back to stripped task id
        "alignment": {"spec": {}},              # non-discover task ignored
    }}))
    eval_runner._write_auto_approve_discoveries(pkg)
    marker = pkg / "runtime" / ".sme-auto-approve-discoveries"
    assert marker.exists()
    data = json.loads(marker.read_text())
    assert set(data["allow"]) == {"alignment", "variant_calling", "no_spec"}
    assert data["deny"] == []


def test_write_auto_approve_no_workflow_json(tmp_path):
    pkg = tmp_path / "pkg"
    pkg.mkdir()
    eval_runner._write_auto_approve_discoveries(pkg)  # must not raise
    data = json.loads((pkg / "runtime" / ".sme-auto-approve-discoveries").read_text())
    assert data["allow"] == ["*"]  # no workflow -> allow-all fallback
    assert data["deny"] == []
