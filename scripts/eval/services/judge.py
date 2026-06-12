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


def _parse_judge_levels(judge_text: str) -> dict:
    """Extract ``{criterion_id_lowercased: "A"|"B"|"C"}`` from a judge response.

    Accepts BOTH the dataset reference scorer's JSON shape and the legacy
    line-based shape, so existing fixtures/caches keep parsing:

    * JSON (``<task>/tests/llm_judge.py`` protocol, mirrored by ``_prompt``):
      ``{"criteria": {"criterion_1": {"level": "A", "reason": "..."}, ...}}``.
      The reference scorer extracts the first brace-balanced object in the
      response (tolerating prose/code-fences around it) and reads each
      criterion's ``"level"`` letter — we reproduce that here.
    * Line-based (legacy ``_prompt`` + the existing cached/fixture verdicts):
      one ``id: A`` line per criterion.

    JSON levels win when present; line matches fill any criterion the JSON
    object omitted. Ids are case-folded so lookup against the rubric id is
    case-insensitive."""
    parsed: dict[str, str] = {}
    # Line-based first (lowest precedence); JSON overlays it below.
    for m in _LINE.finditer(judge_text):
        parsed[m.group(1).lower()] = m.group(2).upper()
    # JSON: extract the first brace-balanced object (same brace-counting walk as
    # the dataset reference scorer) and read criteria[*]["level"].
    obj = _extract_json_object(judge_text)
    if isinstance(obj, dict):
        criteria = obj.get("criteria")
        if isinstance(criteria, dict):
            for cid, c in criteria.items():
                level = None
                if isinstance(c, dict):
                    level = c.get("level")
                elif isinstance(c, str):
                    level = c  # tolerate {"criterion_1": "A"} shorthand
                if isinstance(level, str):
                    lv = level.strip().upper()
                    if lv in ("A", "B", "C"):
                        parsed[str(cid).lower()] = lv
    return parsed


def _extract_json_object(text: str) -> object | None:
    """Return the first brace-balanced JSON object in ``text``, or None.

    Mirrors the dataset reference scorer's parser (``<task>/tests/llm_judge.py``):
    find the first ``{``, walk to its matching ``}`` counting nesting, and
    ``json.loads`` that slice — tolerating leading/trailing prose or ```` ```json ````
    fences the judge model may wrap around the object."""
    start = text.find("{")
    if start == -1:
        return None
    depth = 0
    for i in range(start, len(text)):
        ch = text[i]
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                try:
                    return json.loads(text[start:i + 1])
                except (json.JSONDecodeError, ValueError):
                    return None
    return None


def parse_verdict(rubric: dict, judge_text: str) -> dict:
    """Map per-criterion A/B/C levels to a 0-100 overall + per-dimension scores.

    The judge response is parsed by :func:`_parse_judge_levels`, which accepts
    BOTH the dataset reference scorer's JSON shape
    (``{"criteria": {"criterion_1": {"level": "A"}, ...}}``) and the legacy
    line-based ``id: A`` shape, so existing fixtures and caches keep parsing.

    Two scoring modes, selected by ``rubric.get("scoring")`` (default
    ``"fraction"`` so bare dict rubrics keep their historic behavior):

    * ``"absolute"`` — the sum-and-clamp model the dataset reference scorer
      (``<task>/tests/llm_judge.py``) uses. ``levels`` hold ABSOLUTE per-level
      points (incl. negatives, e.g. the source-reliability penalty
      ``{A:0,B:-5,C:-10}``). The score is the SUM of the chosen levels' points,
      clamped to [0,100] — no division. A perfect run is 100; bad sourcing
      subtracts up to 10. NOTE: the sum-and-clamp math matches the reference, but
      the NEGATIVE penalty does NOT — the reference's `Levels:` regex can't read
      negatives and silently zeroes them, so it never subtracts. We apply the
      paper-documented penalty deliberately (see rubric_normalize.py); the gap
      vs the published 73.34 is documented, not hidden.
    * ``"fraction"`` — synthetic / dict / holistic rubrics. ``levels`` hold
      fractions of each criterion's ``points`` max; the score is the weighted
      percentage ``100 * earned / total``.

    Criterion ids are matched case-insensitively: a judge line `Overall: A` or
    `CRITERION_1: A` still credits ids `overall` / `criterion_1`."""
    # Parsed ids are case-folded so lookup against the rubric id is
    # case-insensitive; the returned `levels` map preserves the rubric's casing.
    parsed = _parse_judge_levels(judge_text)
    absolute = rubric.get("scoring") == "absolute"
    total_pts = 0.0
    earned_pts = 0.0
    dim_total: dict[str, float] = {}
    dim_earned: dict[str, float] = {}
    dim_worst: dict[str, float] = {}
    levels: dict[str, str] = {}
    for c in rubric["criteria"]:
        pts = float(c["points"])
        level = parsed.get(str(c["id"]).lower(), "C")
        levels[c["id"]] = level
        dim = c["dimension"]
        if absolute:
            # `levels` are absolute per-level points; the criterion's max is its
            # A-weight (`points`). Sum points; max bounds the per-dimension roll-up.
            earned = float(c["levels"].get(level, 0.0))
            total_pts += pts
            earned_pts += earned
            dim_total[dim] = dim_total.get(dim, 0.0) + pts
            dim_earned[dim] = dim_earned.get(dim, 0.0) + earned
            # Most-negative level (C for a penalty criterion, 0 normally): the
            # per-dimension floor used to normalize the satisfaction rate.
            dim_worst[dim] = dim_worst.get(dim, 0.0) + min(c["levels"].values())
        else:
            frac = c["levels"].get(level, 0.0)
            total_pts += pts
            earned_pts += pts * frac
            dim_total[dim] = dim_total.get(dim, 0.0) + pts
            dim_earned[dim] = dim_earned.get(dim, 0.0) + pts * frac
    if absolute:
        # Sum-and-clamp: a penalty criterion (A=0) drags a perfect-but-unsourced
        # run below 100; an all-A run sums to 100 exactly.
        overall = max(0.0, min(100.0, earned_pts))
        # Per-dimension SATISFACTION rate, normalized between the dimension's worst
        # and best achievable: (earned - worst)/(best - worst). For a normal
        # dimension (best=A-weights, worst=0) this is the historic earned/best%;
        # for the A=0/B=-5/C=-10 penalty dimension (best=0, worst=-10) it yields
        # A->100 / B->50 / C->0 instead of a misleading flat 0.0%.
        dims = {}
        for d in dim_total:
            best = dim_total[d]
            worst = dim_worst.get(d, 0.0)
            span = best - worst
            if span > 0:
                val = 100.0 * (dim_earned[d] - worst) / span
            else:
                val = 100.0 if dim_earned[d] >= best else 0.0
            dims[d] = max(0.0, min(100.0, val))
    else:
        overall = 100.0 * earned_pts / total_pts if total_pts else 0.0
        dims = {d: (100.0 * dim_earned[d] / dim_total[d] if dim_total[d] else 0.0)
                for d in dim_total}
    rationales = _parse_judge_rationales(judge_text)
    return {"overall": round(overall, 4), "dimensions": dims, "levels": levels,
            "rationales": rationales}


def _parse_judge_rationales(judge_text: str) -> dict:
    """Extract the judge's per-criterion free-text REASONS + the
    ``overall_reasoning`` summary, mirroring :func:`_parse_judge_levels`.

    Returns ``{criterion_id_lowercased: reason, ..., "overall_reasoning": str}``.
    The judge prompt requests ``{"criteria": {"criterion_1": {"level","reason"},
    ...}, "overall_reasoning": "..."}`` (see ``_prompt``); ``parse_verdict``
    historically read only ``level`` and DISCARDED ``reason``, so the *why* behind
    every score was lost from the scorecard (only the raw, hash-named judge cache
    retained it, unlinked to any task). This restores the rationale so it can be
    persisted into ``Score.extra`` and the scorecard for every future run.
    Empty when the response carries no reasons (legacy line-based verdicts)."""
    out: dict[str, str] = {}
    obj = _extract_json_object(judge_text)
    if isinstance(obj, dict):
        criteria = obj.get("criteria")
        if isinstance(criteria, dict):
            for cid, c in criteria.items():
                if isinstance(c, dict):
                    reason = c.get("reason")
                    if isinstance(reason, str) and reason.strip():
                        out[str(cid).lower()] = reason.strip()
        overall = obj.get("overall_reasoning")
        if isinstance(overall, str) and overall.strip():
            out["overall_reasoning"] = overall.strip()
    return out


def _cache_path(judge_id: str, rubric: dict, trace: str, answer: str) -> Path:
    h = hashlib.sha256(
        (judge_id + json.dumps(rubric, sort_keys=True) + trace + answer).encode()
    ).hexdigest()
    d = Path(os.environ.get("ECAA_EVAL_CACHE_DIR",
                            Path.home() / ".ecaa-workflow" / "eval-cache")) / "judge"
    d.mkdir(parents=True, exist_ok=True)
    return d / f"{judge_id}-{h}.txt"


def _criterion_block(c: dict, absolute: bool = True) -> str:
    """Render one rubric criterion the way the dataset reference scorer's
    injected ``rubric.txt`` presents it: id, title/description text, and the
    A/B/C levels with their point values AND their verbatim prose descriptions.

    The dataset scorer (``<task>/tests/llm_judge.py``) injects the verbatim
    ``rubric.txt``, which spells out each level's prose ``[A]/[B]/[C]``
    description AND its ``Levels: A=X B=Y C=0`` point line. ``normalize_rubric``
    now retains those per-level prose lines in ``level_text`` (alongside the
    per-level POINTS in ``levels``), so we present each level's VERBATIM
    description — what the judge is told to grade against — with its point value.
    Synthetic rubrics that carry no ``level_text`` fall back to a generic A/B/C
    semantic so they still render with the dataset's framing.

    ``absolute`` selects the level-value label: in absolute mode each level value
    is points and renders " (N points)"; in fraction mode each level value is a
    fraction-of-max weight (1.0/0.5/0.0) and renders " (weight N)" so a 0.5 weight
    is not mislabelled as "0.5 points". Defaults to absolute so any caller that
    omits it keeps the historic point labelling."""
    lv = c.get("levels", {}) or {}
    lt = c.get("level_text", {}) or {}

    def _pts(letter: str) -> str:
        v = lv.get(letter)
        if v is None:
            return ""
        # {v:g} renders 30.0 as "30" and keeps fractional weights (0.5) readable.
        if absolute:
            return f" ({v:g} points)"
        return f" (weight {v:g})"

    _FALLBACK = {
        "A": "fully correct / best-practice handling of this criterion.",
        "B": "partially correct — a minor mistake or incomplete handling.",
        "C": "skipped, wrong, or unsupported.",
    }

    def _level_line(letter: str) -> str:
        prose = lt.get(letter)
        body = prose.strip() if prose and prose.strip() else _FALLBACK[letter]
        return f"  [{letter}]{_pts(letter)}: {body}"

    return (
        f"{c['id']}: {c.get('text', '')}\n"
        f"{_level_line('A')}\n"
        f"{_level_line('B')}\n"
        f"{_level_line('C')}"
    )


def _prompt(rubric: dict, trace: str, answer: str) -> str:
    """Build the judge prompt, mirroring the BiomniBench-DA reference scorer.

    The dataset reference scorer (``<task>/tests/llm_judge.py``) frames the call
    as an expert data-analysis evaluator, injects the rubric, wraps the agent's
    trace in ``<trace>`` and answer in ``<answer>`` tags, instructs the judge to
    pick ONE level (A/B/C) per criterion "based purely on which level description
    best describes the agent's work", and requires a JSON response of exactly the
    shape::

        {"criteria": {"criterion_1": {"level": "A", "reason": "..."}, ...},
         "overall_reasoning": "..."}

    We reproduce that framing, the ``<trace>``/``<answer>`` tagging, and the exact
    JSON output contract so our judge's A/B/C assignments are faithful to (and our
    scores comparable with) the paper's methodology. :func:`parse_verdict` parses
    this JSON; it also still accepts legacy ``id: A`` lines for back-compat.

    The reference scorer injects the verbatim ``rubric.txt``, including each
    criterion's full prose ``[A]/[B]/[C]`` level *descriptions*.
    ``normalize_rubric`` now retains those per-level prose lines in
    ``level_text``, and :func:`_criterion_block` emits them VERBATIM (with point
    values) — so the discriminating level descriptions the judge is told to grade
    against are present, matching the reference. Synthetic rubrics carrying no
    ``level_text`` fall back to a generic A/B/C semantic. Output format, criterion
    ids, and scoring math are faithful.

    FIDELITY: the prompt prose now matches the BiomniBench-DA reference scorer
    (``<task>/tests/llm_judge.py``) verbatim — "expert evaluator for a data
    analysis task" (no domain qualifier), the "for each criterion choose ONE
    level" instruction placed AFTER the rubric/``<trace>``/``<answer>`` (reference
    order), and the reference's "Here is the agent's analysis trace/final answer"
    framing. The criterion ids are ``criterion_N`` (the reference's scheme), so
    the JSON example carries directly. The one remaining (substance-equivalent)
    difference: the rubric block is rendered from the normalized structure via
    ``_criterion_block`` — same criterion ids, A/B/C prose, and point values the
    reference's verbatim ``rubric.txt`` carries — rather than injected as the raw
    file. Output format and scoring math are identical to the reference."""
    absolute = rubric.get("scoring") == "absolute"
    crit = "\n\n".join(_criterion_block(c, absolute) for c in rubric["criteria"])
    return (
        "You are an expert evaluator for a data analysis task.\n\n"
        "Evaluate the agent's work using the following rubric:\n\n"
        f"{crit}\n\n"
        "Here is the agent's analysis trace:\n\n"
        f"<trace>\n{trace}\n</trace>\n\n"
        "Here is the agent's final answer:\n\n"
        f"<answer>\n{answer}\n</answer>\n\n"
        "For each criterion in the rubric, choose ONE level: A, B, or C — based "
        "purely on which level description best describes the agent's work. Do "
        "not output numerical points; the score for each level is computed "
        "automatically from the rubric.\n\n"
        "You MUST respond with a JSON object in exactly this format:\n"
        "{\n"
        '  "criteria": {\n'
        '    "criterion_1": {"level": "A", "reason": "<one-sentence explanation>"},\n'
        '    "criterion_2": {"level": "B", "reason": "<one-sentence explanation>"}\n'
        "  },\n"
        '  "overall_reasoning": "<short summary>"\n'
        "}\n\n"
        'Each "level" value must be exactly the single character "A", "B", or '
        '"C". Only output the JSON object, nothing else.'
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


def _sync_judge_timeout() -> int:
    """Per-call read timeout (seconds) for a SYNCHRONOUS judge call. The default
    120 s can be too short for a large agent trace (a flattened ECAA package with
    the full final_report.md + claims runs 15+ KB), where the judge's
    time-to-first-byte exceeds 120 s and the read times out. Env-tunable;
    default raised to 300 s. Read at call time so it's per-run adjustable."""
    try:
        return max(30, int(os.environ.get("ECAA_EVAL_JUDGE_TIMEOUT", "300")))
    except ValueError:
        return 300


def _gemini_call(prompt: str) -> tuple[str, int, int]:
    """Return (text, in_tok, out_tok) from a live Gemini call."""
    key = os.environ["GEMINI_API_KEY"]
    url = ("https://generativelanguage.googleapis.com/v1beta/models/"
           f"gemini-3.1-pro-preview:generateContent?key={key}")
    r = requests.post(url, json={"contents": [{"parts": [{"text": prompt}]}],
                                 "generationConfig": {"temperature": 0.0}},
                      timeout=_sync_judge_timeout())
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
    # max_tokens matches the reference scorer's 8192: a 1024 cap truncates the
    # JSON verdict for a 10-criterion rubric (each criterion carries a one-line
    # reason), which then fails to parse and silently defaults every missing
    # criterion to "C" — deflating the score.
    r = requests.post("https://api.anthropic.com/v1/messages",
                      headers={"x-api-key": key, "anthropic-version": "2023-06-01"},
                      json={"model": "claude-opus-4-8", "max_tokens": 8192,
                            "messages": [{"role": "user", "content": prompt}]},
                      timeout=_sync_judge_timeout())
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
                # 8192 matches the reference scorer; 1024 truncates a 10-criterion
                # JSON verdict -> parse failure -> silent all-"C" deflation.
                "max_tokens": 8192,
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
