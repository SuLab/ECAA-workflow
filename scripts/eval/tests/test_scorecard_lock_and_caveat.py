"""eval-03 method-lock asymmetry + eval-05 heuristic-dimension caveat.

Covers the two scorecard auditability fixes:
  * eval-03 — locked (stage, method) pairs are recorded per arm, sourced from
    the plugin's locked_methods contract, and the asymmetry is auditable.
  * eval-05 — the heuristic per-dimension caveat is unmissable in BOTH the JSON
    (explicit dimension_caveat field) and the markdown (loud blockquote).
"""
import json

from scripts.eval.benchmark import Arm, Score, Scorecard
from scripts.eval.plugins.nekrutenko import Nekrutenko
from scripts.eval.services.scorecard import (
    locked_methods_meta,
    write_scorecard,
)


def _two_arm_card(benchmark="nekrutenko", meta=None):
    rows = [
        Score("mtdna", "ecaa", 0, 90.0, {}, 0.9, None, "deterministic"),
        Score("mtdna", "claude-direct", 0, 80.0, {}, 0.8, None, "deterministic"),
    ]
    return Scorecard(benchmark, rows, meta=meta or {})


# ── eval-03: method-lock asymmetry ───────────────────────────────────────────

def test_locked_methods_meta_records_ecaa_pins_and_bare_free():
    card = _two_arm_card()
    meta = locked_methods_meta(Nekrutenko(), card)
    assert meta is not None
    assert meta["ecaa"]["any_locked"] is True
    assert {"stage": "alignment", "method": "bwa"} in meta["ecaa"]["pairs"]
    assert {"stage": "variant_calling", "method": "lofreq"} in meta["ecaa"]["pairs"]
    assert meta["claude-direct"]["any_locked"] is False
    assert meta["claude-direct"]["pairs"] == []
    assert meta["asymmetric"] is True


def test_locked_methods_meta_none_when_no_arm_locks():
    class _FreePlugin:
        def locked_methods(self, task, arm):
            return []

    assert locked_methods_meta(_FreePlugin(), _two_arm_card("biomnibench")) is None


def test_locked_methods_meta_none_without_contract():
    class _NoContract:
        pass

    assert locked_methods_meta(_NoContract(), _two_arm_card()) is None


def test_write_scorecard_threads_locked_methods_into_json_and_md(tmp_path):
    out = write_scorecard(_two_arm_card(), tmp_path, plugin=Nekrutenko())
    data = json.loads((out / "scorecard.json").read_text())
    lm = data["meta"]["locked_methods"]
    assert lm["ecaa"]["any_locked"] is True
    assert lm["claude-direct"]["any_locked"] is False
    assert lm["asymmetric"] is True

    md = (out / "scorecard.md").read_text()
    assert "Method lock" in md
    assert "bwa" in md and "lofreq" in md
    assert "asymmetry" in md.lower()
    # The bare arm row shows free, not pinned.
    assert "_(none — free)_" in md


def test_write_scorecard_without_plugin_omits_locked_methods(tmp_path):
    out = write_scorecard(_two_arm_card(), tmp_path)
    data = json.loads((out / "scorecard.json").read_text())
    assert "locked_methods" not in data["meta"]


def test_nekrutenko_arm_enum_contract_holds():
    # Guard against the Arm enum drifting away from what the helper feeds in.
    plug = Nekrutenko()
    assert plug.locked_methods(None, Arm.ECAA_WORKFLOW) == [
        ("alignment", "bwa"), ("variant_calling", "lofreq")]
    assert plug.locked_methods(None, Arm.CLAUDE_CODE_DIRECT) == []


# ── eval-05: heuristic-dimension caveat ───────────────────────────────────────

def _biomni_card():
    rows = [
        Score("da-1", "ecaa", 0, 70.0, {"method_selection": 60.0}, None, None, "gemini"),
        Score("da-1", "claude-direct", 0, 60.0, {"method_selection": 50.0}, None, None, "gemini"),
    ]
    meta = {
        "dimensions": {
            "ecaa": {"method_selection": 60.0},
            "claude-direct": {"method_selection": 50.0},
        },
        "dimension_source": "heuristic_title_match",
        "dimension_note": "Criteria are bucketed by title-keyword match.",
    }
    return Scorecard("biomnibench", rows, meta=meta)


def test_dimension_caveat_in_json(tmp_path):
    out = write_scorecard(_biomni_card(), tmp_path)
    data = json.loads((out / "scorecard.json").read_text())
    caveat = data["meta"]["dimension_caveat"]
    assert "HEURISTIC" in caveat.upper()
    assert "heuristic_title_match" in caveat
    assert "DO NOT cite" in caveat


def test_dimension_caveat_unmissable_in_markdown(tmp_path):
    out = write_scorecard(_biomni_card(), tmp_path)
    md = (out / "scorecard.md").read_text()
    # Loud blockquote sits inside the Per-dimension section.
    assert "Per-dimension" in md
    assert "HEURISTIC — NOT PAPER-FAITHFUL" in md
    assert "DO NOT cite these per-dimension numbers" in md
    # The blockquote precedes the dimension table.
    assert md.index("HEURISTIC — NOT PAPER-FAITHFUL") < md.index("| dimension |")


def test_no_caveat_when_paper_defined(tmp_path):
    card = _biomni_card()
    card.meta["dimension_source"] = "paper_defined"
    out = write_scorecard(card, tmp_path)
    md = (out / "scorecard.md").read_text()
    assert "HEURISTIC — NOT PAPER-FAITHFUL" not in md
    data = json.loads((out / "scorecard.json").read_text())
    assert "dimension_caveat" not in data["meta"]
