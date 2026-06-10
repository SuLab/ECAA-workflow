"""The committed datasets.lock pins only frozen 40-hex SHAs, and verify_campaign
rejects a scorecard whose provenance datasets-lock carries an unfrozen ref.

This is the B2 pin gate: reruns must pin. The live runner already aborts on an
unpinned ref (services/datasets.py::validate_pins); this guards the committed
file offline and cross-checks a published scorecard's provenance string.
"""
import json
import re
from pathlib import Path

import pytest

from scripts.eval.verify_campaign import verify_run, CampaignViolation

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover
    import tomli as tomllib

_SHA = re.compile(r"^[0-9a-f]{40}$")
_MANIFEST = {
    "campaign": {"seed": 1729, "min_paired_pairs": 10,
                 "arms": ["ecaa", "claude-direct"],
                 "datasets_lock": "scripts/eval/datasets.lock"},
    "benchmarks": [{"name": "nekrutenko", "judge": "deterministic"}],
}


def test_committed_lock_entries_are_frozen_shas():
    p = Path(__file__).resolve().parents[1] / "datasets.lock"
    lock = tomllib.loads(p.read_text())
    assert lock["entries"], "datasets.lock has no entries"
    for e in lock["entries"]:
        assert _SHA.match(e["revision"]), \
            f"{e['name']} revision {e['revision']!r} is not a 40-hex SHA"


def test_verify_rejects_unfrozen_provenance_lock(tmp_path):
    rows = [{"arm": a, "task_id": "t", "overall": 40.0 + i,
             "judge_id": "deterministic"}
            for i, a in enumerate(("ecaa", "claude-direct"))]
    card = {"benchmark": "nekrutenko",
            "meta": {"seed": 1729, "paired_delta": {"n_pairs": 12},
                     "provenance": {"datasets_lock":
                                    "phylobio/BiomniBench-DA=main"}},
            "rows": rows}
    rd = tmp_path / "r"
    rd.mkdir()
    (rd / "scorecard.json").write_text(json.dumps(card))
    with pytest.raises(CampaignViolation, match="datasets_lock|SHA|frozen"):
        verify_run(rd, _MANIFEST)


def test_verify_accepts_frozen_provenance_lock(tmp_path):
    # A frozen 40-hex provenance lock string passes the (5) cross-check.
    rows = [{"arm": a, "task_id": "t", "overall": 40.0 + i,
             "judge_id": "deterministic"}
            for i, a in enumerate(("ecaa", "claude-direct"))]
    frozen = ("phylobio/BiomniBench-DA="
              "810b6c54a81e98019bb6c36bdbdc1d4e93dd46d1;"
              "nekrut/LLM-eval-paper="
              "1175f72a998aa609958504571830294e1401a16e")
    card = {"benchmark": "nekrutenko",
            "meta": {"seed": 1729, "paired_delta": {"n_pairs": 12},
                     "provenance": {"datasets_lock": frozen}},
            "rows": rows}
    rd = tmp_path / "r"
    rd.mkdir()
    (rd / "scorecard.json").write_text(json.dumps(card))
    report = verify_run(rd, _MANIFEST)
    assert report["compliant"] is True
