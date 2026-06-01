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


def _render_error_matrix(em: dict) -> list[str]:
    """Render meta["error_matrix"] — one line per arm plus a by-pattern table."""
    lines = ["", "## Error matrix", ""]
    arms = sorted(em.keys())
    for arm in arms:
        entry = em[arm]
        lines.append(
            f"- {arm}: recover {entry.get('recover_rate', 0.0):.3f},"
            f" diagnose {entry.get('diagnose_rate', 0.0):.3f}"
            f" (n={entry.get('n_cells', 0)})"
        )
    lines.append("")
    # Collect union of patterns across all arms.
    all_patterns: list[str] = []
    seen: set[str] = set()
    for arm in arms:
        for pat in em[arm].get("by_pattern", {}):
            if pat not in seen:
                all_patterns.append(pat)
                seen.add(pat)
    if all_patterns:
        # Header: pattern | <arm> recover | <arm> diagnose (repeated per arm)
        header_cols = ["pattern"]
        sep_cols = ["---"]
        for arm in arms:
            header_cols += [f"{arm} recover", f"{arm} diagnose"]
            sep_cols += ["---", "---"]
        lines.append("| " + " | ".join(header_cols) + " |")
        lines.append("| " + " | ".join(sep_cols) + " |")
        for pat in all_patterns:
            row_cols = [pat]
            for arm in arms:
                bp = em[arm].get("by_pattern", {}).get(pat)
                if bp:
                    row_cols += [
                        f"{bp.get('recover_rate', 0.0):.3f}",
                        f"{bp.get('diagnose_rate', 0.0):.3f}",
                    ]
                else:
                    row_cols += ["", ""]
            lines.append("| " + " | ".join(row_cols) + " |")
    return lines


def _render_dimensions(meta: dict) -> list[str]:
    """Render meta["dimensions"] (BiomniBench) as a per-dimension table."""
    dims_meta: dict = meta["dimensions"]
    arms = sorted(dims_meta.keys())
    # Collect union of dimension names in insertion order.
    all_dims: list[str] = []
    seen: set[str] = set()
    for arm in arms:
        for dim in dims_meta[arm]:
            if dim not in seen:
                all_dims.append(dim)
                seen.add(dim)

    lines = ["", "## Per-dimension", ""]
    ecaa_vals = dims_meta.get("ecaa", {})
    direct_vals = dims_meta.get("claude-direct", {})

    lines.append("| dimension | ecaa | claude-direct | delta |")
    lines.append("| --- | --- | --- | --- |")
    for dim in all_dims:
        e = ecaa_vals.get(dim)
        d = direct_vals.get(dim)
        e_str = f"{e:.1f}" if e is not None else ""
        d_str = f"{d:.1f}" if d is not None else ""
        if e is not None and d is not None:
            delta_str = f"{e - d:+.1f}"
        else:
            delta_str = ""
        lines.append(f"| {dim} | {e_str} | {d_str} | {delta_str} |")

    if "published_best" in meta:
        lines.append("")
        lines.append(f"Published best: {meta['published_best']}")

    return lines


def _render_judge_agreement(ja: dict) -> list[str]:
    exact = ja.get("exact", "")
    kappa = ja.get("kappa", "")
    return [f"Inter-judge agreement: exact {exact}, linear-weighted kappa {kappa}"]


def _markdown(card: Scorecard) -> str:
    lines = [f"# {card.benchmark} scorecard", ""]
    # Render scalar meta keys (skip the rich-object keys handled below).
    _RICH_KEYS = {"error_matrix", "dimensions", "judge_agreement", "published_best", "cost"}
    if card.meta:
        for k, v in card.meta.items():
            if k not in _RICH_KEYS:
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

    # Optional rich sections.
    if card.meta:
        if "error_matrix" in card.meta:
            lines += _render_error_matrix(card.meta["error_matrix"])
        if "dimensions" in card.meta:
            lines += _render_dimensions(card.meta)
        if "judge_agreement" in card.meta:
            lines.append("")
            lines += _render_judge_agreement(card.meta["judge_agreement"])
        if "cost" in card.meta:
            cost = card.meta["cost"] or {}
            lines.append("")
            lines.append(f"Judge cost (USD): {cost.get('judge_usd', '')}")

    return "\n".join(lines) + "\n"


def write_scorecard(card: Scorecard, out_dir: Path) -> Path:
    out_dir.mkdir(parents=True, exist_ok=True)
    payload = {"benchmark": card.benchmark, "meta": card.meta,
               "rows": [asdict(r) for r in card.rows]}
    (out_dir / "scorecard.json").write_text(json.dumps(payload, indent=2, default=str))
    (out_dir / "scorecard.md").write_text(_markdown(card))
    return out_dir
