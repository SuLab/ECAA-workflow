from scripts.eval.plugins import biomnibench
from scripts.eval.plugins.biomnibench import (
    compute_intra_narrative_self_consistency,
    _find_nested,
)


def test_renamed_metric_returns_self_consistency_shape():
    out = compute_intra_narrative_self_consistency("TP53 up 2.0-fold.", "TP53\t2.0")
    assert set(out) == {"verified_count", "total_claims",
                        "verified_pct", "reference_type"}
    assert out["verified_count"] == 1


def test_legacy_name_aliased_for_backcompat():
    # Old symbol still importable but delegates to the renamed fn.
    assert biomnibench.compute_claim_groundedness is \
        biomnibench.compute_intra_narrative_self_consistency


def test_find_nested_excludes_staged_inputs(tmp_path):
    # Real deliverable at run root...
    (tmp_path / "answer.txt").write_text("real")
    # ...and an incidental same-named file inside the staged inputs subtree.
    staged = tmp_path / "inputs" / "dataset"
    staged.mkdir(parents=True)
    (staged / "answer.txt").write_text("staged-decoy")
    # _find_nested excludes the root itself, so it must NOT return the staged
    # decoy now that inputs/ is excluded — it returns None (caller falls back
    # to the root-level answer.txt).
    hit = _find_nested(tmp_path, "answer.txt")
    assert hit is None


def test_find_nested_still_recovers_app_nested_deliverable(tmp_path):
    app = tmp_path / "app"
    app.mkdir()
    (app / "trace.md").write_text("nested deliverable")
    hit = _find_nested(tmp_path, "trace.md")
    assert hit == app / "trace.md"


def test_find_nested_prefers_non_input_over_input(tmp_path):
    # A genuine nested deliverable AND an inputs/ decoy both exist; the inputs/
    # one is excluded entirely, so the app/ one is returned.
    app = tmp_path / "app"
    app.mkdir()
    (app / "answer.txt").write_text("real-nested")
    staged = tmp_path / "inputs" / "ds"
    staged.mkdir(parents=True)
    (staged / "answer.txt").write_text("decoy")
    hit = _find_nested(tmp_path, "answer.txt")
    assert hit == app / "answer.txt"
