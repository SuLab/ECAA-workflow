"""LLM-as-judge for BiomniBench: Gemini 3.1 Pro (headline, paper-faithful)
+ Anthropic (cross-check). Verdict parsing is pure + fully tested; HTTP is
cached by sha256(rubric, trace, answer) to avoid re-billing on rerun.
"""
from __future__ import annotations
import hashlib
import json
import os
import re
import sys
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


def _gemini_batch(items: list[dict]) -> dict[str, tuple[str, int, int]]:
    """Submit one Gemini batch and poll until done.

    ``items`` is a list of dicts with keys "key" and "prompt".
    Returns ``{key: (text, in_tok, out_tok)}``.

    Uses the Gemini Batch API inline-requests path:
      POST https://generativelanguage.googleapis.com/v1beta/models/gemini-3.1-pro-preview:batchGenerateContent
    The model is encoded in the URL path (not a per-request field).
    Each inline request carries a metadata.key for correlation.
    Poll GET v1beta/{name} until state == "JOB_STATE_SUCCEEDED",
    then read response.inlinedResponses[] paired with input keys.
    See: https://ai.google.dev/gemini-api/docs/batch-api
    """
    import time
    key = os.environ["GEMINI_API_KEY"]
    model = "gemini-3.1-pro-preview"
    base = "https://generativelanguage.googleapis.com/v1beta"

    # Build inline batch payload. Model goes in URL, not per-request body.
    requests_payload = [
        {
            "request": {"contents": [{"parts": [{"text": item["prompt"]}]}],
                        "generationConfig": {"temperature": 0.0}},
            "metadata": {"key": item["key"]},
        }
        for item in items
    ]
    r = requests.post(
        f"{base}/models/{model}:batchGenerateContent",
        params={"key": key},
        json={"batch": {
            "display_name": "ecaa-eval-judge",
            "input_config": {"requests": {"requests": requests_payload}},
        }},
        timeout=60,
    )
    r.raise_for_status()
    # "name" is a full resource path like "batches/123456789".
    batch_name = r.json()["name"]

    # Poll until succeeded or failed.
    while True:
        time.sleep(30)
        poll = requests.get(f"{base}/{batch_name}", params={"key": key}, timeout=60)
        poll.raise_for_status()
        body = poll.json()
        state = body.get("state", "")
        if state == "JOB_STATE_SUCCEEDED":
            break
        if state in ("JOB_STATE_FAILED", "JOB_STATE_CANCELLED", "JOB_STATE_EXPIRED"):
            raise RuntimeError(f"Gemini batch ended with state {state}")

    # Pair inlinedResponses with original keys (same order as submitted).
    # Completed response shape: {"name":…, "state":…, "response": {"inlinedResponses": […]}}
    inlined = body.get("response", {}).get("inlinedResponses", [])
    results: dict[str, tuple[str, int, int]] = {}
    for item, resp_obj in zip(items, inlined):
        # Each element is a GenerateContentResponse (or a status object on error).
        text = (resp_obj.get("candidates", [{}])[0]
                        .get("content", {})
                        .get("parts", [{}])[0]
                        .get("text", ""))
        usage = resp_obj.get("usageMetadata", {})
        in_tok = usage.get("promptTokenCount", 0)
        out_tok = usage.get("candidatesTokenCount", 0)
        results[item["key"]] = (text, in_tok, out_tok)
    return results


def _anthropic_batch(items: list[dict]) -> dict[str, tuple[str, int, int]]:
    """Submit one Anthropic Message Batch and poll until ended.

    ``items`` is a list of dicts with keys "key" and "prompt".
    Returns ``{key: (text, in_tok, out_tok)}``.

    Uses the Anthropic Message Batches API:
      POST https://api.anthropic.com/v1/messages/batches
    Each request carries a custom_id == our key (sanitised to match ^[a-zA-Z0-9_-]{1,64}$).
    Poll GET .../messages/batches/{id} until processing_status == "ended",
    then stream results from results_url.
    See: https://platform.claude.com/docs/en/build-with-claude/batch-processing
    """
    import time
    api_key = os.environ["ECAA_ANTHROPIC_API_KEY"]
    headers = {
        "x-api-key": api_key,
        "anthropic-version": "2023-06-01",
        "content-type": "application/json",
    }

    # Build batch payload; custom_id must match ^[a-zA-Z0-9_-]{1,64}$.
    # We sanitise by replacing disallowed chars with '_' and truncating.
    def _safe_id(raw: str) -> str:
        sanitised = re.sub(r"[^a-zA-Z0-9_-]", "_", raw)
        return sanitised[:64]

    key_map: dict[str, str] = {}  # safe_id -> original key
    requests_payload = []
    for item in items:
        safe = _safe_id(item["key"])
        # Avoid collisions by appending index if needed.
        idx = 0
        candidate = safe
        while candidate in key_map:
            candidate = (safe[:60] + f"_{idx}")[:64]
            idx += 1
        key_map[candidate] = item["key"]
        requests_payload.append({
            "custom_id": candidate,
            "params": {
                "model": "claude-opus-4-8",
                "max_tokens": 1024,
                "messages": [{"role": "user", "content": item["prompt"]}],
            },
        })

    r = requests.post(
        "https://api.anthropic.com/v1/messages/batches",
        headers=headers,
        json={"requests": requests_payload},
        timeout=60,
    )
    r.raise_for_status()
    batch_id = r.json()["id"]

    # Poll until ended.
    while True:
        time.sleep(60)
        poll = requests.get(
            f"https://api.anthropic.com/v1/messages/batches/{batch_id}",
            headers=headers,
            timeout=60,
        )
        poll.raise_for_status()
        body = poll.json()
        if body.get("processing_status") == "ended":
            break

    # Retrieve results from results_url (JSONL streamed).
    results_url = body.get("results_url", "")
    resp = requests.get(results_url, headers=headers, timeout=120, stream=True)
    resp.raise_for_status()

    results: dict[str, tuple[str, int, int]] = {}
    for line in resp.iter_lines():
        if not line:
            continue
        row = json.loads(line)
        custom_id = row.get("custom_id", "")
        original_key = key_map.get(custom_id, custom_id)
        result = row.get("result", {})
        if result.get("type") == "succeeded":
            msg = result.get("message", {})
            content = msg.get("content", [{}])
            text = content[0].get("text", "") if content else ""
            usage = msg.get("usage", {})
            in_tok = usage.get("input_tokens", 0)
            out_tok = usage.get("output_tokens", 0)
        else:
            text = ""
            in_tok = 0
            out_tok = 0
        results[original_key] = (text, in_tok, out_tok)
    return results


def _batch_min() -> int:
    """Minimum cache-miss count (per provider) to use the batch API instead of
    synchronous calls. Read at call time so it is env-tunable per run."""
    try:
        return max(1, int(os.environ.get("ECAA_EVAL_JUDGE_BATCH_MIN", "16")))
    except ValueError:
        return 16


def _fetch_provider(misses: list[dict], batch_fn, sync_fn) -> dict[str, tuple[str, int, int]]:
    """Fetch verdict text for one provider's cache-misses.

    Below ECAA_EVAL_JUDGE_BATCH_MIN requests, call the synchronous API per
    request — seconds of latency at full price, the right trade for dry-runs and
    small runs. At/above the threshold submit one provider batch — ~50% cheaper
    but minutes-to-hours of latency, the right trade for full runs. (Even a tiny
    batch can take 30+ min, so a small run must never be forced through it.)
    Returns {key: (text, in_tok, out_tok)}.
    """
    if len(misses) >= _batch_min():
        return batch_fn(misses)
    return {m["key"]: sync_fn(m["prompt"]) for m in misses}


def _resolve_provider(misses: list[dict], batch_fn, sync_fn,
                      results: dict[str, dict]) -> None:
    """Fetch + cache + score one provider's cache-misses into ``results``.

    Each verdict is cached only after a successful fetch, so a provider failure
    (which raises out of this function) leaves nothing cached for that provider —
    ``--resume`` then retries exactly the unscored requests."""
    fetched = _fetch_provider(misses, batch_fn, sync_fn)
    for entry in misses:
        key = entry["key"]
        text, in_tok, out_tok = fetched[key]
        _cache_path(entry["judge_id"], entry["rubric"],
                    entry["trace"], entry["answer"]).write_text(text)
        verdict = parse_verdict(entry["rubric"], text)
        verdict["cost_usd"] = _judge_cost_usd(entry["judge_id"], in_tok, out_tok)
        results[key] = verdict


def judge_batch(requests_list: list[dict]) -> dict[str, dict]:
    """Score a list of judge requests in batch, grouped by provider.

    Each request dict must have keys:
      "key"      — unique string identifying this request in the return map
      "judge_id" — "gemini-3.1-pro" or "anthropic-opus"
      "rubric"   — rubric dict
      "trace"    — trace text
      "answer"   — answer text

    Returns ``{key: {overall, dimensions, levels, cost_usd}}`` for all requests,
    with cache hits at cost_usd=0.0 and fetched items carrying real cost.

    Cache hits are partitioned out before any HTTP call; only misses are batched.
    """
    results: dict[str, dict] = {}
    misses_gemini: list[dict] = []   # {key, prompt, rubric, judge_id}
    misses_anthropic: list[dict] = []

    for req in requests_list:
        key = req["key"]
        judge_id = req["judge_id"]
        rubric = req["rubric"]
        trace = req["trace"]
        answer = req["answer"]

        cache = _cache_path(judge_id, rubric, trace, answer)
        if cache.exists():
            text = cache.read_text()
            verdict = parse_verdict(rubric, text)
            verdict["cost_usd"] = 0.0
            results[key] = verdict
        else:
            prompt = _prompt(rubric, trace, answer)
            entry = {"key": key, "prompt": prompt, "rubric": rubric,
                     "judge_id": judge_id, "trace": trace, "answer": answer}
            if judge_id == "gemini-3.1-pro":
                misses_gemini.append(entry)
            else:
                misses_anthropic.append(entry)

    # Fetch misses per provider, each fault-isolated: a provider that fails
    # (e.g. out of credits) is logged and skipped — the other still scores, and
    # the failed provider is left un-cached so --resume retries only those.
    # Sync below ECAA_EVAL_JUDGE_BATCH_MIN, batch at/above it.
    for provider, misses, batch_fn, sync_fn in (
        ("gemini-3.1-pro", misses_gemini, _gemini_batch, _gemini_call),
        ("anthropic-opus", misses_anthropic, _anthropic_batch, _anthropic_call),
    ):
        if not misses:
            continue
        try:
            _resolve_provider(misses, batch_fn, sync_fn, results)
        except Exception as e:
            print(f"[judge] provider {provider} failed; {len(misses)} request(s) "
                  f"left unscored + un-cached (resume to retry): {e}", file=sys.stderr)

    return results


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
