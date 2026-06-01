"""eval-02: the unattended eval auto-advances ONLY the discovery review gate
(the human-in-the-loop SME step with no claude-direct analog) and KEEPS the
silent-completion / missing-artifact / validation / claim-verification guards
ACTIVE so ECAA's error-catching is measured, not hidden."""
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


# ── _write_auto_approve_discovery_gate: discovery-gate-only scope ────────────


def _workflow(tasks: dict) -> str:
    return json.dumps({"tasks": tasks})


def test_discovery_gate_does_not_write_blanket_skip_decisions(tmp_path):
    """eval-02 regression: the OLD bypass wrote a `skip_with_deviation`
    `sme-decisions.json` for EVERY task, which neutered the harness silent-
    completion / missing-artifact / validation guards. The new policy must write
    NO such files — those guards must run their strict path."""
    pkg = tmp_path / "pkg"
    pkg.mkdir()
    (pkg / "WORKFLOW.json").write_text(_workflow({
        "discover_variant_calling": {"spec": {"stage_class": "variant_calling"}},
        "review_prior_work": {"spec": {}},
        "variant_calling": {"spec": {}},
        "reporting": {"spec": {}},
    }))
    eval_runner._write_auto_approve_discovery_gate(pkg)

    # NO task gets an sme-decisions.json (the guard-bypass file).
    for tid in ("discover_variant_calling", "review_prior_work",
                "variant_calling", "reporting"):
        dec = pkg / "runtime" / "outputs" / tid / "sme-decisions.json"
        assert not dec.exists(), (
            f"{tid} got a guard-bypassing sme-decisions.json — the silent-"
            f"completion/missing-artifact/validation guards would be disabled"
        )


def test_discovery_gate_writes_review_confirmed_sidecars_for_discover_only(tmp_path):
    pkg = tmp_path / "pkg"
    pkg.mkdir()
    (pkg / "WORKFLOW.json").write_text(_workflow({
        "discover_alignment": {"spec": {"stage_class": "alignment"}},
        "discover_variant_calling": {"spec": {"stage_class": "variant_calling"}},
        "alignment": {"spec": {}},        # non-discover -> no sidecar
        "review_prior_work": {"spec": {}},
    }))
    eval_runner._write_auto_approve_discovery_gate(pkg)

    runtime = pkg / "runtime"
    # discover_* tasks get a sme-review-confirmed sidecar (review gate clear).
    for tid in ("discover_alignment", "discover_variant_calling"):
        sidecar = runtime / f"sme-review-confirmed-{tid}.json"
        assert sidecar.exists(), f"missing review-confirmed sidecar for {tid}"
        data = json.loads(sidecar.read_text())
        assert data["stage"] == tid
        assert data["auto_approved"] is True
    # non-discover tasks get NO review-confirmed sidecar.
    assert not (runtime / "sme-review-confirmed-alignment.json").exists()
    assert not (runtime / "sme-review-confirmed-review_prior_work.json").exists()


def test_discovery_gate_still_writes_discoveries_marker(tmp_path):
    pkg = tmp_path / "pkg"
    pkg.mkdir()
    (pkg / "WORKFLOW.json").write_text(_workflow({
        "discover_alignment": {"spec": {"stage_class": "alignment"}},
        "review_prior_work": {"spec": {}},
    }))
    eval_runner._write_auto_approve_discovery_gate(pkg)

    marker = pkg / "runtime" / ".sme-auto-approve-discoveries"
    assert marker.exists()
    data = json.loads(marker.read_text())
    assert data["allow"] == ["alignment"]
    assert data["deny"] == []


def test_discovery_gate_no_workflow_json(tmp_path):
    # No WORKFLOW.json: must not raise, still writes the allow-all discoveries
    # marker, and writes no per-task files (no task ids to iterate).
    pkg = tmp_path / "pkg"
    pkg.mkdir()
    eval_runner._write_auto_approve_discovery_gate(pkg)  # must not raise
    marker = pkg / "runtime" / ".sme-auto-approve-discoveries"
    assert json.loads(marker.read_text())["allow"] == ["*"]
    outputs = pkg / "runtime" / "outputs"
    assert not outputs.exists() or not any(outputs.iterdir())


def test_legacy_alias_points_at_discovery_gate_only(tmp_path):
    """The old name `_write_auto_approve_all` is kept as an alias but now has
    the discovery-gate-only behaviour (no blanket guard bypass)."""
    assert (eval_runner._write_auto_approve_all
            is eval_runner._write_auto_approve_discovery_gate)
    pkg = tmp_path / "pkg"
    pkg.mkdir()
    (pkg / "WORKFLOW.json").write_text(_workflow({"reporting": {"spec": {}}}))
    eval_runner._write_auto_approve_all(pkg)
    assert not (pkg / "runtime" / "outputs" / "reporting" / "sme-decisions.json").exists()


def test_read_workflow_task_ids_malformed(tmp_path):
    pkg = tmp_path / "pkg"
    pkg.mkdir()
    # absent -> []
    assert eval_runner._read_workflow_task_ids(pkg) == []
    # malformed -> []
    (pkg / "WORKFLOW.json").write_text("{not json")
    assert eval_runner._read_workflow_task_ids(pkg) == []
    # well-formed -> task ids
    (pkg / "WORKFLOW.json").write_text(_workflow({"a": {}, "b": {}}))
    assert set(eval_runner._read_workflow_task_ids(pkg)) == {"a", "b"}
