"""Generic flattener: emitted package runtime outputs -> trace.md + answer.txt.

trace.md  = per-task narratives concatenated in topological DAG order.
answer.txt = the narrative of the terminal reporting task (stage contains
'report'/'review', else the last task in topo order).

WORKFLOW.json shape contract
-----------------------------
The Rust emitter serialises ``tasks`` as a JSON OBJECT keyed by task id
(Rust ``BTreeMap<TaskId, Task>``), and the per-task objects carry NO ``id``
or ``stage`` field — only ``kind``/``state``/``depends_on``/``assignee``/
``description`` (and occasionally ``spec``/``source_atom_id``/
``required_artifacts``/``container``). The task id (the map key) IS the stage
name (``alignment``, ``discover_alignment``, ``final_reporting``, …), so we
use the key as both the task id and the stage signal. ``eval_runner.py``
reads ``tasks`` the same way (``data.get("tasks", {}).items()``).
"""
from __future__ import annotations
import json
import os
from pathlib import Path

_NARRATIVE_NAMES = ("final_report.md", "report.md", "interpretation.md", "summary.md", "result.md")
_RESULT_JSON_KEYS = ("narrative", "interpretation", "summary", "report", "answer", "text")

# Extension -> markdown code-fence language for surfacing executed code into the
# trace. The keys are the on-disk script suffixes the agent writes under
# runtime/outputs/<tid>/scripts/; the suffix allowlist is also the guard that
# keeps non-code siblings (.log/.lock/.json) out of the rendered block.
# Suffix -> code-fence language. Mirrors the emitter's GeneratedCode completion
# gate (*.{py,R,sh,smk}) so a task whose real mechanism is a Snakemake rule is
# surfaced rather than silently dropped. Snakemake is Python-syntax.
_CODE_FENCE_LANG = {".py": "python", ".r": "r", ".R": "r", ".sh": "bash", ".smk": "python"}
_CODE_EXTS = tuple(_CODE_FENCE_LANG)
# Generous per-file cap: real analysis scripts are far smaller (a DESeq2 script
# is ~9 KB), so this never truncates genuine mechanism logic, but it bounds a
# pathological long script from ballooning the judged trace. Truncation is
# marked, never silent.
_MAX_CODE_BYTES_PER_FILE = 48_000
# agent-code.json `language` tag -> code-fence language for the fallback path.
_AGENT_CODE_LANG = {"Python": "python", "R": "r", "Bash": "bash"}


def _normalize_tasks(tasks) -> dict[str, dict]:
    """Coerce the WORKFLOW.json ``tasks`` payload into an ``{id: task}`` dict.

    The emitter writes an object keyed by task id (the canonical shape). A
    legacy/list shape (``[{"id": ..., "depends_on": ...}, ...]``) is tolerated
    so older fixtures and any hand-written WORKFLOW.json keep flattening — each
    entry's ``id`` becomes the key. Anything else yields an empty mapping.
    """
    if isinstance(tasks, dict):
        return {str(tid): (obj if isinstance(obj, dict) else {})
                for tid, obj in tasks.items()}
    if isinstance(tasks, list):
        out: dict[str, dict] = {}
        for entry in tasks:
            if isinstance(entry, dict) and "id" in entry:
                out[str(entry["id"])] = entry
        return out
    return {}


def _stage_of(task_id: str, task: dict) -> str:
    """Best-effort stage label for a task.

    The object-keyed shape has no ``stage`` field, so the task id is the
    stage (``alignment``, ``final_reporting``, …). A legacy ``stage`` field
    or a ``spec.stage_class`` is honoured when present.
    """
    stage = task.get("stage")
    if isinstance(stage, str) and stage:
        return stage
    spec = task.get("spec")
    if isinstance(spec, dict):
        sc = spec.get("stage_class")
        if isinstance(sc, str) and sc:
            return sc
    return task_id


def _topo(tasks: dict[str, dict]) -> list[str]:
    seen, order = set(), []

    def visit(tid):
        if tid in seen:
            return
        seen.add(tid)
        for dep in tasks.get(tid, {}).get("depends_on", []) or []:
            visit(str(dep))
        order.append(tid)

    for tid in tasks:
        visit(tid)
    return order


def _narrative(task_dir: Path) -> str:
    # (a) well-known report markdown FIRST — the agent's human-facing
    #     deliverable (full interpretation + code + mechanism), which is what
    #     the rubric grades. A reporting task writes both a rich
    #     `final_report.md`/`report.md` AND a `result.json` whose `narrative`
    #     field is a COMPRESSED operational digest; preferring the markdown
    #     surfaces the mechanism-deep report to the judge instead of the digest.
    #     Only reporting stages carry these files, so analytical tasks (which
    #     have only result.json) are unaffected and keep their result.json
    #     narrative. Skip an empty/whitespace markdown so it can't shadow a
    #     good result.json.
    for name in _NARRATIVE_NAMES:
        p = task_dir / name
        if p.exists():
            txt = p.read_text()
            if txt.strip():
                return txt

    # (b) result.json — known narrative keys, else full JSON dump.
    rj = task_dir / "result.json"
    if rj.exists():
        try:
            data = json.loads(rj.read_text())
            if isinstance(data, dict):
                for key in _RESULT_JSON_KEYS:
                    val = data.get(key)
                    if isinstance(val, str) and val.strip():
                        return val
                return json.dumps(data, indent=2)
        except (json.JSONDecodeError, OSError):
            pass

    # (c) any *.md in the dir (sorted)
    mds = sorted(task_dir.glob("*.md"))
    if mds:
        return mds[0].read_text()

    # (d) progress.log
    pl = task_dir / "progress.log"
    if pl.exists():
        return pl.read_text()

    return ""


def _terminal_id(order: list[str], stage: dict[str, str]) -> str | None:
    """Resolve the terminal task whose narrative is the workflow's answer.

    On the real emitted DAG the topo order interleaves leaf ``validate_*``
    tasks *after* the substantive reporting tasks they check (each validator
    depends on its target), so a naive "last task containing report/review"
    lands on ``validate_review_prior_work`` rather than ``final_reporting``.
    Resolution, in priority order:

    1. The last topo-ordered *reporting* task that is itself neither a
       ``validate_*`` validator nor a ``discover_*`` method-selection stub
       (``final_reporting`` / ``reporting``). This is the real answer-bearing
       task.
    2. Failing that, the last topo-ordered non-validator/non-discover task
       whose stage mentions ``review`` (e.g. a workflow whose only terminal is
       ``review_prior_work``).
    3. Failing that, the last non-validator/non-discover task in topo order.
    4. Failing that (degenerate DAG), the last task in topo order.
    """
    if not order:
        return None

    def _substantive(tid: str) -> bool:
        return not (tid.startswith("validate_") or tid.startswith("discover_"))

    def _last(pred) -> str | None:
        chosen = None
        for tid in order:
            if pred(tid):
                chosen = tid
        return chosen

    report = _last(
        lambda tid: _substantive(tid) and "report" in stage.get(tid, "")
    )
    if report is not None:
        return report

    review = _last(
        lambda tid: _substantive(tid) and "review" in stage.get(tid, "")
    )
    if review is not None:
        return review

    substantive_tail = _last(_substantive)
    if substantive_tail is not None:
        return substantive_tail

    return order[-1]


def _result_json_claims_block(task_dir: Path) -> str:
    """Render the terminal task's `result.json` `claims` as a compact block.

    The report markdown is prose; `result.json.claims` carry the structured,
    machine-checkable findings WITH per-claim evidence pointers (and exact
    intermediate counts the prose may omit, e.g. "89,408 baseline tumour
    cells"). Appending them to the terminal narrative surfaces that
    rubric-relevant detail (intermediate counts + traceability) to the judge
    without dropping the rich prose. Empty/absent claims yield ""."""
    rj = task_dir / "result.json"
    if not rj.exists():
        return ""
    try:
        data = json.loads(rj.read_text())
    except (json.JSONDecodeError, OSError):
        return ""
    claims = data.get("claims") if isinstance(data, dict) else None
    if not isinstance(claims, list) or not claims:
        return ""
    lines = ["", "### Structured claims (machine-checkable, with evidence)"]
    for c in claims:
        if isinstance(c, dict):
            txt = c.get("claim") or c.get("text") or ""
            ev = c.get("evidence") or ""
            if txt:
                lines.append(f"- {txt}" + (f"  [evidence: {ev}]" if ev else ""))
        elif isinstance(c, str) and c.strip():
            lines.append(f"- {c}")
    return "\n".join(lines) if len(lines) > 2 else ""


def _fence_code(lang: str, code: str) -> str:
    """Wrap one script in a fenced block, capped at ``_MAX_CODE_BYTES_PER_FILE``.

    Real analysis scripts sit well under the cap, so genuine mechanism logic is
    never lost; truncation only guards a pathological long file and is marked
    explicitly (never silent) so the trace stays honest about completeness.
    """
    body = code.rstrip()
    if len(body) > _MAX_CODE_BYTES_PER_FILE:
        body = body[:_MAX_CODE_BYTES_PER_FILE] + "\n# … [truncated for trace length]"
    return f"```{lang}\n{body}\n```"


def _executed_code_block(task_dir: Path) -> str:
    """Render the task's OWN executed code as fenced blocks for the trace.

    The agent writes its real, copy-pasteable scripts to
    ``runtime/outputs/<tid>/scripts/*.{py,R,sh}`` at execution time; these are
    the durable, on-disk record of the mechanism that produced the results
    (e.g. the DESeq2/fgsea/OLS scripts). The graded trace is otherwise pure
    prose, so the judge must take the method on faith — surfacing the code
    closes that fidelity gap and brings the ECAA arm to parity with the bare
    arm, which already inlines its code per step (the rubric requires shown,
    executable code of BOTH arms). This is faithful surfacing of code the agent
    actually ran, not augmentation: nothing is fabricated, and the same
    requirement applies to both arms.

    Source priority:
      1. On-disk ``scripts/*.{py,R,sh}`` — the reliable source. Globbed
         non-recursively (no nested subdirs) and filtered by the ``_CODE_EXTS``
         suffix allowlist so coexisting ``.log``/``.lock``/``.json`` siblings
         are skipped. Files are read in sorted order for determinism within a
         run.
      2. FALLBACK (forward-compatible): when no script files exist, read
         ``agent-code.json`` and surface a non-empty ``executed_code`` field.
         Today ``executed_code`` is empty in practice (the agent log is a
         single-line JSON blob with no parsable code), so this is a no-op for
         current packages, but it keeps the trace correct if the CLI ever
         exposes a transcript that populates the field.

    The header is the neutral ``### Executed code`` — no exhaustive-
    reproducibility claim is made (the surfaced scripts are the persisted
    executable record, not a guarantee of every inline snippet). Returns ""
    when neither source yields code. Never raises — IO/JSON errors yield "".
    """
    blocks: list[str] = []
    scripts_dir = task_dir / "scripts"
    if scripts_dir.is_dir():
        try:
            files = sorted(
                p for p in scripts_dir.iterdir()
                if p.is_file() and p.suffix in _CODE_EXTS
            )
        except OSError:
            files = []
        for p in files:
            try:
                code = p.read_text()
            except OSError:
                continue
            if code.strip():
                blocks.append(_fence_code(_CODE_FENCE_LANG[p.suffix], code))

    if not blocks:
        # Fallback: no on-disk scripts/ — surface agent-code.json if populated.
        ac = task_dir / "agent-code.json"
        if ac.exists():
            try:
                data = json.loads(ac.read_text())
            except (json.JSONDecodeError, OSError):
                data = None
            if isinstance(data, dict):
                code = data.get("executed_code")
                if isinstance(code, str) and code.strip():
                    lang = _AGENT_CODE_LANG.get(data.get("language", ""), "")
                    blocks.append(_fence_code(lang, code))

    if not blocks:
        return ""
    return "\n\n### Executed code\n\n" + "\n\n".join(blocks)


def _augment_enabled() -> bool:
    """Per-run narrative augmentation toggle. Default OFF so scored runs feed
    the judge the SAME raw narrative on both arms (H1 fairness): the ECAA-only
    structured-claims block is augmentation the bare arm cannot receive. Opt in
    with ECAA_EVAL_NARRATIVE_AUGMENT=1 for diagnostics only."""
    return os.environ.get("ECAA_EVAL_NARRATIVE_AUGMENT", "0") == "1"


def flatten_outputs(outputs_dir: Path, workflow_json: Path) -> tuple[str, str]:
    tasks = _normalize_tasks(json.loads(Path(workflow_json).read_text())["tasks"])
    order = _topo(tasks)
    stage = {tid: _stage_of(tid, task) for tid, task in tasks.items()}
    sections = []
    terminal_id = _terminal_id(order, stage)
    augment = _augment_enabled()
    for tid in order:
        narr = _narrative(outputs_dir / tid)
        # The terminal report gets its structured claims appended ONLY when
        # augmentation is explicitly opted in (default OFF for fair scoring):
        # exact intermediate counts + evidence pointers the prose can omit, but
        # detail the bare arm cannot receive.
        if augment and tid == terminal_id:
            narr = (narr + _result_json_claims_block(outputs_dir / tid)).strip()
        # Surface the task's OWN executed code into its trace section,
        # UNCONDITIONALLY (not behind the augment toggle): the bare arm already
        # inlines its code per step and the rubric requires shown code of BOTH
        # arms, so this removes an asymmetry rather than augmenting one arm.
        code_block = _executed_code_block(outputs_dir / tid)
        if code_block:
            narr = (narr + code_block).strip()
        if narr:
            sections.append(f"## Task: {tid} ({stage.get(tid,'')})\n\n{narr}")
    trace = "\n\n".join(sections)
    answer = ""
    if terminal_id:
        answer = _narrative(outputs_dir / terminal_id)
        if augment:
            answer = (answer + _result_json_claims_block(
                outputs_dir / terminal_id)).strip()
        else:
            answer = answer.strip()
    # Fallback: a complete trace must NEVER yield an empty answer channel. When
    # the resolved terminal produced no narrative (e.g. a goal-driven DAG with no
    # reporting terminal, so the terminal resolves to a leaf/validate_* with an
    # empty output dir), fall back to the richest non-empty narrative among the
    # SUBSTANTIVE tasks (skip validate_*/discover_*). A complete-but-empty-terminal
    # analysis (e.g. cross-omics ending at druggable_target_prioritization) would
    # otherwise score 0 purely because the answer channel was empty. Length tie ->
    # the later (downstream) task wins, so the analytical tail beats early QC. Raw
    # narrative only (no augment claims-block) to preserve H1 arm fairness.
    if not answer:
        best = ""
        for tid in order:
            if tid.startswith("validate_") or tid.startswith("discover_"):
                continue
            narr = _narrative(outputs_dir / tid).strip()
            if narr and len(narr) >= len(best):
                best = narr
        answer = best
    return trace, answer


def completion_status(outputs_dir: Path, workflow_json: Path) -> dict:
    """Report how much of the workflow actually produced output.

    Returns ``{"total": N, "with_output": M, "terminal_has_output": bool}``:

    * ``total``    — number of tasks declared in WORKFLOW.json.
    * ``with_output`` — how many of those tasks have a non-empty narrative under
      ``runtime/outputs/<id>/`` (per ``_narrative``).
    * ``terminal_has_output`` — whether the resolved terminal/reporting task
      produced a non-empty narrative.

    Used to distinguish a workflow that stalled mid-run (and therefore has an
    empty/partial answer) from one that completed but scored poorly. Never
    raises; on a malformed/missing WORKFLOW.json returns an all-zero status.
    """
    try:
        tasks = _normalize_tasks(json.loads(Path(workflow_json).read_text())["tasks"])
    except (json.JSONDecodeError, OSError, KeyError, TypeError):
        return {"total": 0, "with_output": 0, "terminal_has_output": False}

    order = _topo(tasks)
    stage = {tid: _stage_of(tid, task) for tid, task in tasks.items()}
    with_output = sum(1 for tid in order if _narrative(outputs_dir / tid).strip())
    terminal_id = _terminal_id(order, stage)
    terminal_has_output = bool(
        terminal_id and _narrative(outputs_dir / terminal_id).strip()
    )
    return {
        "total": len(order),
        "with_output": with_output,
        "terminal_has_output": terminal_has_output,
    }
