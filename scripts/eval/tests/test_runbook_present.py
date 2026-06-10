"""The operator two-arm campaign runbook is committed under docs/ecaa-spec/
(the one tracked docs subtree) and covers both arms, the verify gate, the
fairness knobs, and the value-prose gate.
"""
from pathlib import Path


def test_runbook_exists_and_covers_both_arms():
    p = (Path(__file__).resolve().parents[3]
         / "docs" / "ecaa-spec" / "eval-campaign-runbook.md")
    assert p.exists(), "operator runbook missing"
    txt = p.read_text()
    for token in ("ECAA_EVAL_LIVE=1", "verify_campaign", "ecaa",
                  "claude-direct", "ECAA_EVAL_ALLOW_RELAUNCH",
                  "ECAA_EVAL_NARRATIVE_AUGMENT", "value-prose"):
        assert token in txt, f"runbook missing required token: {token}"
