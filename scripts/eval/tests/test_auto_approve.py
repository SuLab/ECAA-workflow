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


# ── _write_auto_approve_all: broader unattended bypass ──────────────────────
#
# The discoveries marker alone only clears the discover_* review gate. The
# observed live hang was a NON-discovery task (review_prior_work) flipped
# completed -> blocked [validation_failed] by the harness-guard, whose only
# generic bypass is runtime/outputs/<task_id>/sme-decisions.json carrying a
# skip option id. These tests pin that _write_auto_approve_all writes every
# marker/decision file the shipped harness honors.

# Canonical skip-option ids the harness recognizes
# (crates/harness/src/sme_skip.rs::SKIP_OPTION_IDS). The chosen value MUST be
# one of these or detect_intent returns None and the guard re-blocks anyway.
_HARNESS_SKIP_OPTION_IDS = {
    "emit_skip_sentinel_row",
    "mark_task_failed_documented_deviation",
    "drop_stage_from_workflow",
    "skip_with_deviation",
    "skip_with_documented_deviation",
}


def _workflow(tasks: dict) -> str:
    return json.dumps({"tasks": tasks})


def test_write_auto_approve_all_writes_sme_decisions_for_every_task(tmp_path):
    pkg = tmp_path / "pkg"
    pkg.mkdir()
    (pkg / "WORKFLOW.json").write_text(_workflow({
        "discover_variant_calling": {"spec": {"stage_class": "variant_calling"}},
        "review_prior_work": {"spec": {}},
        "variant_calling": {"spec": {}},
        "reporting": {"spec": {}},
    }))
    eval_runner._write_auto_approve_all(pkg)

    # Every task gets a sme-decisions.json with a recognized skip option id so
    # the harness-guard re-block (validation/sentinel/missing-artifact) is
    # bypassed and a real completion is kept.
    for tid in ("discover_variant_calling", "review_prior_work",
                "variant_calling", "reporting"):
        dec = pkg / "runtime" / "outputs" / tid / "sme-decisions.json"
        assert dec.exists(), f"missing sme-decisions.json for {tid}"
        data = json.loads(dec.read_text())
        assert data["task_id"] == tid
        chosen = {d["chosen"] for d in data["decisions"]}
        assert chosen, f"no decision rows for {tid}"
        assert chosen <= _HARNESS_SKIP_OPTION_IDS, (
            f"{tid} chose {chosen}, not in harness SKIP_OPTION_IDS "
            f"{_HARNESS_SKIP_OPTION_IDS} — detect_intent would return None"
        )


def test_write_auto_approve_all_writes_review_confirmed_sidecars(tmp_path):
    pkg = tmp_path / "pkg"
    pkg.mkdir()
    (pkg / "WORKFLOW.json").write_text(_workflow({
        "discover_alignment": {"spec": {"stage_class": "alignment"}},
        "discover_variant_calling": {"spec": {"stage_class": "variant_calling"}},
        "alignment": {"spec": {}},        # non-discover -> no sidecar
    }))
    eval_runner._write_auto_approve_all(pkg)

    runtime = pkg / "runtime"
    # discover_* tasks get a sme-review-confirmed sidecar (review gate clear).
    for tid in ("discover_alignment", "discover_variant_calling"):
        sidecar = runtime / f"sme-review-confirmed-{tid}.json"
        assert sidecar.exists(), f"missing review-confirmed sidecar for {tid}"
        data = json.loads(sidecar.read_text())
        assert data["stage"] == tid
        assert data["auto_approved"] is True
    # non-discover task gets NO review-confirmed sidecar.
    assert not (runtime / "sme-review-confirmed-alignment.json").exists()


def test_write_auto_approve_all_still_writes_discoveries_marker(tmp_path):
    pkg = tmp_path / "pkg"
    pkg.mkdir()
    (pkg / "WORKFLOW.json").write_text(_workflow({
        "discover_alignment": {"spec": {"stage_class": "alignment"}},
        "review_prior_work": {"spec": {}},
    }))
    eval_runner._write_auto_approve_all(pkg)

    marker = pkg / "runtime" / ".sme-auto-approve-discoveries"
    assert marker.exists()
    data = json.loads(marker.read_text())
    assert data["allow"] == ["alignment"]
    assert data["deny"] == []


def test_write_auto_approve_all_no_workflow_json(tmp_path):
    # No WORKFLOW.json: must not raise, still writes the allow-all discoveries
    # marker, and writes no per-task files (no task ids to iterate).
    pkg = tmp_path / "pkg"
    pkg.mkdir()
    eval_runner._write_auto_approve_all(pkg)  # must not raise
    marker = pkg / "runtime" / ".sme-auto-approve-discoveries"
    assert json.loads(marker.read_text())["allow"] == ["*"]
    outputs = pkg / "runtime" / "outputs"
    assert not outputs.exists() or not any(outputs.iterdir())


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
