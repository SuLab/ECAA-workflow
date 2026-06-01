"""LLM-as-judge for BiomniBench: Gemini 3.1 Pro (headline, paper-faithful)
+ Anthropic (cross-check). Verdict parsing is pure + fully tested; HTTP is
cached by sha256(rubric, trace, answer) to avoid re-billing on rerun.
"""
from __future__ import annotations
import hashlib
import json
import os
import re
from pathlib import Path

import requests

_LINE = re.compile(r"^[\s\-*]*([A-Za-z0-9_]+)\s*[:=]\s*([ABCabc])\b", re.MULTILINE)


def parse_verdict(rubric: dict, judge_text: str) -> dict:
    """Map per-criterion A/B/C levels to 0-100 overall + per-dimension."""
    levels = {m.group(1): m.group(2).upper() for m in _LINE.finditer(judge_text)}
    total_pts = 0.0
    earned_pts = 0.0
    dim_total: dict[str, float] = {}
    dim_earned: dict[str, float] = {}
    for c in rubric["criteria"]:
        pts = float(c["points"])
        frac = c["levels"].get(levels.get(c["id"], "C"), 0.0)
        total_pts += pts
        earned_pts += pts * frac
        dim_total[c["dimension"]] = dim_total.get(c["dimension"], 0.0) + pts
        dim_earned[c["dimension"]] = dim_earned.get(c["dimension"], 0.0) + pts * frac
    overall = 100.0 * earned_pts / total_pts if total_pts else 0.0
    dims = {d: (100.0 * dim_earned[d] / dim_total[d] if dim_total[d] else 0.0)
            for d in dim_total}
    return {"overall": round(overall, 4), "dimensions": dims, "levels": levels}


def _cache_path(judge_id: str, rubric: dict, trace: str, answer: str) -> Path:
    h = hashlib.sha256(
        (judge_id + json.dumps(rubric, sort_keys=True) + trace + answer).encode()
    ).hexdigest()
    d = Path(os.environ.get("ECAA_EVAL_CACHE_DIR",
                            Path.home() / ".ecaa-workflow" / "eval-cache")) / "judge"
    d.mkdir(parents=True, exist_ok=True)
    return d / f"{judge_id}-{h}.txt"


def _prompt(rubric: dict, trace: str, answer: str) -> str:
    crit = "\n".join(f"{c['id']} ({c['dimension']}): {c.get('text','')}"
                     for c in rubric["criteria"])
    return (
        "You are grading a bioinformatics analysis trajectory against a rubric.\n"
        "For each criterion choose A (fully correct), B (minor mistake/partial), "
        "or C (skipped/wrong). Output ONLY one line per criterion as `id: A`.\n\n"
        f"CRITERIA:\n{crit}\n\nTRACE:\n{trace}\n\nANSWER:\n{answer}\n"
    )


_JUDGE_PRICES: dict[str, tuple[float, float]] = {
    # (in_price_per_MTok, out_price_per_MTok)
    "gemini-3.1-pro": (1.25, 5.00),      # maps to gemini-3.1-pro-preview on the API
    "anthropic-opus": (15.0, 75.0),
}


def _judge_cost_usd(judge_id: str, in_tok: int, out_tok: int) -> float:
    """Return estimated USD cost for one judge call given token counts."""
    prices = _JUDGE_PRICES.get(judge_id)
    if prices is None:
        return 0.0
    in_price, out_price = prices
    return in_tok / 1e6 * in_price + out_tok / 1e6 * out_price


def _gemini_call(prompt: str) -> tuple[str, int, int]:
    """Return (text, in_tok, out_tok) from a live Gemini call."""
    key = os.environ["GEMINI_API_KEY"]
    url = ("https://generativelanguage.googleapis.com/v1beta/models/"
           f"gemini-3.1-pro-preview:generateContent?key={key}")
    r = requests.post(url, json={"contents": [{"parts": [{"text": prompt}]}],
                                 "generationConfig": {"temperature": 0.0}}, timeout=120)
    r.raise_for_status()
    body = r.json()
    text = body["candidates"][0]["content"]["parts"][0]["text"]
    usage = body.get("usageMetadata", {})
    in_tok = usage.get("promptTokenCount", 0)
    out_tok = usage.get("candidatesTokenCount", 0)
    return text, in_tok, out_tok


def _anthropic_call(prompt: str) -> tuple[str, int, int]:
    """Return (text, in_tok, out_tok) from a live Anthropic call."""
    key = os.environ["ECAA_ANTHROPIC_API_KEY"]
    r = requests.post("https://api.anthropic.com/v1/messages",
                      headers={"x-api-key": key, "anthropic-version": "2023-06-01"},
                      json={"model": "claude-opus-4-8", "max_tokens": 1024,
                            "messages": [{"role": "user", "content": prompt}]},
                      timeout=120)
    r.raise_for_status()
    body = r.json()
    text = body["content"][0]["text"]
    usage = body.get("usage", {})
    in_tok = usage.get("input_tokens", 0)
    out_tok = usage.get("output_tokens", 0)
    return text, in_tok, out_tok


def judge(judge_id: str, rubric: dict, trace: str, answer: str) -> dict:
    """judge_id in {"gemini-3.1-pro","anthropic-opus"}. Returns parse_verdict dict + cost_usd."""
    cache = _cache_path(judge_id, rubric, trace, answer)
    if cache.exists():
        text = cache.read_text()
        cost_usd = 0.0
    else:
        prompt = _prompt(rubric, trace, answer)
        if judge_id == "gemini-3.1-pro":
            text, in_tok, out_tok = _gemini_call(prompt)
        else:
            text, in_tok, out_tok = _anthropic_call(prompt)
        cache.write_text(text)
        cost_usd = _judge_cost_usd(judge_id, in_tok, out_tok)
    result = parse_verdict(rubric, text)
    result["cost_usd"] = cost_usd
    return result
