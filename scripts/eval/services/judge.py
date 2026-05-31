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


def _gemini_text(prompt: str) -> str:
    key = os.environ["GEMINI_API_KEY"]
    url = ("https://generativelanguage.googleapis.com/v1beta/models/"
           f"gemini-3.1-pro:generateContent?key={key}")
    r = requests.post(url, json={"contents": [{"parts": [{"text": prompt}]}],
                                 "generationConfig": {"temperature": 0.0}}, timeout=120)
    r.raise_for_status()
    return r.json()["candidates"][0]["content"]["parts"][0]["text"]


def _anthropic_text(prompt: str) -> str:
    key = os.environ["ECAA_ANTHROPIC_API_KEY"]
    r = requests.post("https://api.anthropic.com/v1/messages",
                      headers={"x-api-key": key, "anthropic-version": "2023-06-01"},
                      json={"model": "claude-opus-4-8", "max_tokens": 1024,
                            "temperature": 0.0,
                            "messages": [{"role": "user", "content": prompt}]},
                      timeout=120)
    r.raise_for_status()
    return r.json()["content"][0]["text"]


def judge(judge_id: str, rubric: dict, trace: str, answer: str) -> dict:
    """judge_id in {"gemini-3.1-pro","anthropic-opus"}. Returns parse_verdict dict."""
    cache = _cache_path(judge_id, rubric, trace, answer)
    if cache.exists():
        text = cache.read_text()
    else:
        prompt = _prompt(rubric, trace, answer)
        text = _gemini_text(prompt) if judge_id == "gemini-3.1-pro" else _anthropic_text(prompt)
        cache.write_text(text)
    return parse_verdict(rubric, text)
