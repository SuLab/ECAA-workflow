"""Emit machine (JSON) + human (markdown) scorecards.

Nondeterministic fields (timestamps, cost, wall-clock) live under
meta/extra so reruns diff cleanly on substantive fields.
"""
from __future__ import annotations
import json
from dataclasses import asdict
from pathlib import Path
from statistics import mean, pstdev
from scripts.eval.benchmark import Scorecard


def _by_arm(card: Scorecard) -> dict[str, list[float]]:
    out: dict[str, list[float]] = {}
    for r in card.rows:
        out.setdefault(r.arm, []).append(r.overall)
    return out


def _markdown(card: Scorecard) -> str:
    lines = [f"# {card.benchmark} scorecard", ""]
    if card.meta:
        for k, v in card.meta.items():
            lines.append(f"- **{k}:** {v}")
        lines.append("")
    arms = _by_arm(card)
    lines += ["| arm | n | mean | sd |", "|---|---|---|---|"]
    for arm, vals in sorted(arms.items()):
        sd = pstdev(vals) if len(vals) > 1 else 0.0
        lines.append(f"| {arm} | {len(vals)} | {mean(vals):.1f} | {sd:.1f} |")
    lines.append("")
    if "ecaa" in arms and "claude-direct" in arms:
        delta = mean(arms["ecaa"]) - mean(arms["claude-direct"])
        lines.append(f"**ecaa - claude-direct delta:** {delta:+.1f}")
    return "\n".join(lines) + "\n"


def write_scorecard(card: Scorecard, out_dir: Path) -> Path:
    out_dir.mkdir(parents=True, exist_ok=True)
    payload = {"benchmark": card.benchmark, "meta": card.meta,
               "rows": [asdict(r) for r in card.rows]}
    (out_dir / "scorecard.json").write_text(json.dumps(payload, indent=2, default=str))
    (out_dir / "scorecard.md").write_text(_markdown(card))
    return out_dir
