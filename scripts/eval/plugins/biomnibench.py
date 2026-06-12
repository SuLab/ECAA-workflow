"""BiomniBench-DA (process-level, LLM-judged) — 50 public tasks.

ECAA arm: question -> ecaa-workflow intake -> package -> harness; flatten
runtime outputs to trace.md+answer.txt. Direct arm: bare Claude Code on the
question. Scored by Gemini 3.1 Pro (headline) + Anthropic (cross-check).
"""
from __future__ import annotations
import json
import re
from pathlib import Path
from statistics import mean
from scripts.eval.benchmark import Arm, Benchmark, Output, RunSpec, Score, Scorecard, Task
from scripts.eval.rubric_normalize import normalize_rubric
from scripts.eval.scoring.agreement import per_criterion_exact, linear_weighted_kappa
from scripts.eval.scoring.flatten import completion_status, flatten_outputs
from scripts.eval.services.datasets import load_records, stage_file
from scripts.eval.services.judge import judge

# BiomniBench-DA per-task directory layout (da-{paper}-{task}/):
#   instruction.md  -> question text   (loaded into "question" by load_records)
#   tests/rubric.txt -> rubric text     (loaded into "rubric" by load_records)
#   environment/data/ -> data file refs (loaded into "data_files" by load_records)
_F_QUESTION = "question"
_F_RUBRIC = "rubric"
_F_DATA = "data_files"

_OUTPUT_CONTRACT = (
    "\n\nWrite your analytical narrative (decisions, intermediate findings, "
    "interpretation) to trace.md and a short structured answer to answer.txt."
)

def _find_nested(root: Path, name: str) -> Path | None:
    """Shallowest descendant named ``name`` under ``root`` (excluding the root
    itself AND the staged ``inputs/`` subtree), or None. Excluding ``inputs/``
    stops a deeper incidental same-named file inside a staged dataset from
    shadowing the agent's real deliverable (L11). Sorted by path depth so
    ``<workdir>/app/answer.txt`` is recovered before any deeper match."""
    matches = [
        p for p in root.rglob(name)
        if p.is_file()
        and p.parent != root
        and "inputs" not in p.relative_to(root).parts
    ]
    if not matches:
        return None
    return min(matches, key=lambda p: (len(p.relative_to(root).parts), str(p)))


def _read_first(*candidates) -> str:
    """First readable, non-empty text among ``candidates`` (paths or None)."""
    for c in candidates:
        if c is not None and c.is_file():
            text = c.read_text()
            if text.strip():
                return text
    return ""


_CONTAMINATION_DIRECTIVE = (
    "## Evaluation integrity\n"
    "This is a benchmark task. Base every result solely on the data files "
    "provided in this package. Do NOT search for, fetch, or rely on the original "
    "source publication for this dataset, its figures or tables, or any external "
    "answer key; derive all findings from your own analysis of the provided data. "
    "You may consult tool/library documentation, but not look up this task's "
    "answers."
)

# Defense-in-depth for the bare arm: some agents (notably codex/gpt-5.5) assume
# an absolute `/app` deliverable path, then can't create it as a non-root
# container user and silently nest the files under `<cwd>/app/`. Pin the output
# location to the current working directory with the exact relative filenames.
# Collect still recovers nested files, so this only reduces the failure rate.
_BARE_PATH_DIRECTIVE = (
    "\n\nIMPORTANT: write trace.md and answer.txt directly in your current "
    "working directory using exactly those relative filenames. Do not use an "
    "absolute path such as /app and do not create subdirectories for them."
)


# ── claim-groundedness (HEURISTIC visibility metric, NOT a gate) ──────────────
#
# NOISE CAVEAT: extraction is sentence-level keyword heuristics, not the Rust
# claim_verifier. A claim "counts" iff a salient token (number / identifier /
# PMID) re-appears in the flattened result rows. Both false positives (an
# incidental token match) and false negatives (a paraphrased magnitude) are
# expected. The scorecard renders this beside the Gemini headline as a
# value-visibility signal — never to block a run or override the judge.

# Sentences carrying any of these markers are treated as load-bearing CLAIMS.
_CLAIM_MARKERS: tuple[str, ...] = (
    "-fold", "fold change", "fold-change", "fold ",
    "increase", "decrease", "upregulat", "downregulat",
    "higher", "lower", "elevat", "reduc",
    "significant", "p=", "p =", "p<", "p <", "p-value", "p value",
    "correlat", "enrich", "associat", "differen",
)

# Salient tokens we attempt to re-find in the result rows: floats/ints,
# UPPERCASE gene-symbol-shaped tokens, and PMIDs.
_NUM_RE = re.compile(r"-?\d+(?:\.\d+)?")
_GENE_RE = re.compile(r"\b[A-Z][A-Z0-9]{2,}\b")
_PMID_RE = re.compile(r"\bPMID[:\s]*?(\d{4,9})\b", re.IGNORECASE)


def _extract_claims(narrative: str) -> list[str]:
    """Split ``narrative`` into sentences and keep only those bearing a
    quantitative / directional / comparative claim marker. HEURISTIC."""
    if not narrative or not narrative.strip():
        return []
    # Sentence split on ./!/? boundaries followed by whitespace; tolerant of
    # decimals because the markers, not the period, decide inclusion.
    raw = re.split(r"(?<=[.!?])\s+", narrative.strip())
    claims: list[str] = []
    for sent in raw:
        s = sent.strip()
        if not s:
            continue
        low = s.lower()
        if any(m in low for m in _CLAIM_MARKERS):
            claims.append(s)
    return claims


def _grounding_reference_type(*, has_row: bool, has_pmid: bool) -> str:
    """Classify the evidence surface a run's verified claims landed on."""
    if has_row and has_pmid:
        return "mixed"
    if has_pmid:
        return "pmid"
    # Default (incl. no-evidence) is the primary result-row reference surface.
    return "result_row"


def _claim_tokens(claim: str) -> tuple[list[str], list[str]]:
    """Return (non_pmid_tokens, pmid_tokens) salient for grounding a claim."""
    pmids = _PMID_RE.findall(claim)
    nums = _NUM_RE.findall(claim)
    genes = [g for g in _GENE_RE.findall(claim) if g != "PMID"]
    return (nums + genes, pmids)


def compute_intra_narrative_self_consistency(narrative: str,
                                             result_text: str) -> dict:
    """Intra-narrative self-consistency over a flattened run (renamed from
    compute_claim_groundedness; L10).

    This is NOT verification against ground truth — the judge never sees a gold
    answer. A claim "counts" iff a salient token (number / gene-shaped
    identifier / PMID) from the run's OWN narrative re-appears in the run's OWN
    result text. It measures whether the narrative is internally consistent with
    the rows it reports, mirroring the SPIRIT of the Rust ``claim_verifier`` but
    heuristically and judge-independently. (When a real ``claim_verifier``
    sidecar is present its verdicts are the authoritative source; this remains
    the offline fallback the scorecard renders as a visibility signal.) Returns
    the shared Score.extra["intra_narrative_self_consistency"] shape.
    HEURISTIC — see module caveat."""
    claims = _extract_claims(narrative)
    total = len(claims)
    if total == 0:
        return {"verified_count": 0, "total_claims": 0,
                "verified_pct": 0.0, "reference_type": "result_row"}
    haystack = result_text or ""
    haystack_pmids = set(_PMID_RE.findall(haystack))
    verified = 0
    matched_via_row = False
    matched_via_pmid = False
    for claim in claims:
        non_pmid, pmids = _claim_tokens(claim)
        hit_pmid = any(p in haystack_pmids for p in pmids)
        hit_row = any(tok and tok in haystack for tok in non_pmid)
        if hit_pmid or hit_row:
            verified += 1
            matched_via_pmid = matched_via_pmid or hit_pmid
            matched_via_row = matched_via_row or hit_row
    pct = round(100.0 * verified / total, 1)
    return {
        "verified_count": verified,
        "total_claims": total,
        "verified_pct": pct,
        "reference_type": _grounding_reference_type(
            has_row=matched_via_row, has_pmid=matched_via_pmid),
    }


# Backward-compat alias: existing call sites + fixtures may still reference the
# old name; it delegates to the renamed function unchanged.
compute_claim_groundedness = compute_intra_narrative_self_consistency


class BiomniBench(Benchmark):
    @property
    def name(self) -> str:
        return "biomnibench"

    def contamination_directive(self) -> str:
        return _CONTAMINATION_DIRECTIVE

    def proposal_policy(self, task, arm):
        """Open eval: sign off (promote) the LLM's gap-fill nodes so the ECAA
        arm exercises its full compose-and-extend behavior rather than emitting
        a clipped DAG — there's no pinned recipe to preserve here."""
        return "signoff"

    def fetch(self, cache_dir: Path) -> Path:
        from scripts.eval.services.datasets import load_lock, ensure
        lock = Path(__file__).resolve().parents[1] / "datasets.lock"
        return ensure(load_lock(lock)["phylobio/BiomniBench-DA"])

    def tasks(self, handle: Path, *, smoke: bool):
        records = self._load_records(handle)          # list[dict] per Step 1 probe
        if smoke:
            records = records[:2]
        return [self._to_task(handle, r) for r in records]

    def _load_records(self, handle: Path) -> list[dict]:
        return load_records(handle)

    def _to_task(self, handle: Path, r: dict) -> Task:
        tid = r.get("id") or r.get("task_id")
        inputs = {Path(f).name: handle / f for f in r.get(_F_DATA, [])}
        return Task(task_id=str(tid), prompt=r[_F_QUESTION], inputs=inputs,
                    rubric=normalize_rubric(r[_F_RUBRIC]), answer_key=None, meta={})

    def build_run(self, task, arm, workdir):
        workdir.mkdir(parents=True, exist_ok=True)
        if arm == Arm.ECAA_WORKFLOW:
            return RunSpec(arm, workdir, "ecaa_package", task.prompt + _OUTPUT_CONTRACT)
        for name, src in task.inputs.items():
            if src.exists():
                stage_file(src, workdir / name)
        return RunSpec(arm, workdir, "bare",
                       task.prompt + _OUTPUT_CONTRACT + "\n\n"
                       + _CONTAMINATION_DIRECTIVE + _BARE_PATH_DIRECTIVE)

    def collect(self, spec, run_dir):
        artifacts: dict = {}
        if spec.kind == "ecaa_package":
            outputs_dir = run_dir / "runtime" / "outputs"
            workflow_json = run_dir / "WORKFLOW.json"
            trace, answer = flatten_outputs(outputs_dir, workflow_json)
            # Distinguish a stalled/incomplete workflow (empty answer because the
            # terminal task never ran) from one that completed but scored poorly.
            status = completion_status(outputs_dir, workflow_json)
            if not status["terminal_has_output"] or status["with_output"] < status["total"]:
                reason = (
                    "terminal task produced no output"
                    if not status["terminal_has_output"]
                    else "workflow incomplete"
                )
                artifacts["incomplete_reason"] = (
                    f"{reason} "
                    f"({status['with_output']}/{status['total']} tasks completed)"
                )
        else:
            trace_path = run_dir / "trace.md"
            answer_path = run_dir / "answer.txt"
            # Prefer top-level deliverables, then recover ones an agent nested a
            # directory or two down. Codex (gpt-5.5) assumes an absolute `/app`
            # output path, can't mkdir at the container root as a non-root user,
            # and falls back to `<workdir>/app/{trace,answer}`. The shallowest
            # match wins so a staged dataset that happens to contain a same-named
            # file can't shadow the agent's real deliverable.
            trace = _read_first(trace_path, _find_nested(run_dir, "trace.md"))
            answer = _read_first(answer_path, _find_nested(run_dir, "answer.txt"))
            if not trace and not answer:
                stdout_path = run_dir / "agent-stdout.json"
                if stdout_path.exists():
                    raw = stdout_path.read_text()
                    try:
                        parsed = json.loads(raw)
                        answer = parsed.get("result", parsed.get("text", raw))
                    except json.JSONDecodeError:
                        answer = raw
                    trace = raw
                else:
                    trace = ""
                    answer = ""
        return Output(trace_md=trace, answer_txt=answer, artifacts=artifacts,
                      exit_ok=True, wall_secs=0.0)

    def judge_requests(self, task, arm, output):
        """Return two batch judge requests per output: Gemini headline + Anthropic cross."""
        return [
            {
                "role": "headline",
                "judge_id": "gemini-3.1-pro",
                "rubric": task.rubric,
                "trace": output.trace_md,
                "answer": output.answer_txt,
            },
            {
                "role": "cross",
                "judge_id": "anthropic-opus",
                "rubric": task.rubric,
                "trace": output.trace_md,
                "answer": output.answer_txt,
            },
        ]

    def assemble_score(self, task, arm, output, trial, verdicts):
        """Build a Score from pre-fetched judge verdicts.

        Gemini headline is the primary score when present; if Gemini is absent
        (e.g. out of credits) the Opus cross-check becomes the primary so the run
        still yields a usable score, flagged ``partial_judging``. Inter-judge
        agreement is computed only when both providers are present."""
        headline = verdicts.get("headline")
        cross = verdicts.get("cross")
        primary = headline or cross
        judge_id = "gemini-3.1-pro" if headline else "anthropic-opus"
        gemini_cost = headline.get("cost_usd", 0.0) if headline else 0.0
        anthropic_cost = cross.get("cost_usd", 0.0) if cross else 0.0
        extra = {"judge_cost_usd": gemini_cost + anthropic_cost,
                 "gemini_cost_usd": gemini_cost,
                 "anthropic_cost_usd": anthropic_cost}
        # Judge-independent intra-narrative self-consistency visibility metric.
        # Computed from the run's own narrative vs its result rows, regardless of
        # which judge(s) scored it.
        extra["intra_narrative_self_consistency"] = \
            compute_intra_narrative_self_consistency(
                output.trace_md, output.answer_txt)
        if output.artifacts.get("incomplete_reason"):
            extra["incomplete_reason"] = output.artifacts["incomplete_reason"]
        if headline and cross:
            extra["cross_check"] = cross["overall"]
            extra["judge_exact"] = per_criterion_exact(
                headline.get("levels", {}), cross.get("levels", {}))
            extra["judge_kappa"] = linear_weighted_kappa(
                headline.get("levels", {}), cross.get("levels", {}))
        else:
            extra["partial_judging"] = True
        # Persist the judge's free-text rationale (per-criterion reasons +
        # overall_reasoning) so every scorecard row records WHY it scored as it
        # did — recoverable for future RCA without re-running the judge. Keyed by
        # the actual scoring judge (primary) plus the cross-check when present.
        if primary and primary.get("rationales"):
            extra["judge_rationale"] = primary["rationales"]
        if headline and cross and cross.get("rationales"):
            extra["cross_judge_rationale"] = cross["rationales"]
        # Persist per-criterion A/B/C levels so a scorecard reader can see WHICH
        # criterion drove each per-dimension mean and at what level per arm — the
        # per-dimension numbers are a single-criterion title-keyword heuristic at
        # this n, so the driving level is the honest unit of comparison.
        if primary and primary.get("levels"):
            extra["judge_levels"] = primary["levels"]
        if headline and cross and cross.get("levels"):
            extra["cross_judge_levels"] = cross["levels"]
        return Score(task_id=task.task_id, arm=arm.value, trial=trial,
                     overall=primary["overall"], dimensions=primary["dimensions"],
                     jaccard=None, error_cells=None, judge_id=judge_id, extra=extra)

    def score(self, task, arm, output, trial):
        headline = judge("gemini-3.1-pro", task.rubric, output.trace_md, output.answer_txt)
        cross = judge("anthropic-opus", task.rubric, output.trace_md, output.answer_txt)
        exact = per_criterion_exact(headline.get("levels", {}), cross.get("levels", {}))
        kappa = linear_weighted_kappa(headline.get("levels", {}), cross.get("levels", {}))
        return Score(task_id=task.task_id, arm=arm.value, trial=trial,
                     overall=headline["overall"], dimensions=headline["dimensions"],
                     jaccard=None, error_cells=None, judge_id="gemini-3.1-pro",
                     extra={"cross_check": cross["overall"],
                            "judge_exact": exact,
                            "judge_kappa": kappa,
                            "judge_rationale": headline.get("rationales", {}),
                            "cross_judge_rationale": cross.get("rationales", {}),
                            "judge_levels": headline.get("levels", {}),
                            "cross_judge_levels": cross.get("levels", {}),
                            "intra_narrative_self_consistency":
                                compute_intra_narrative_self_consistency(
                                    output.trace_md, output.answer_txt),
                            "judge_cost_usd": headline.get("cost_usd", 0.0) + cross.get("cost_usd", 0.0)})

    def report(self, scores):
        dims: dict[str, dict[str, list[float]]] = {}
        exact_vals: list[float] = []
        kappa_vals: list[float] = []
        for s in scores:
            # Exclude Opus-only fallback rows from the Gemini-headline dimension
            # means (consistent with the scorecard's _by_arm / paired-delta
            # exclusion); they're surfaced via meta.partial_judging_excluded.
            if s.extra.get("partial_judging"):
                continue
            for d, v in s.dimensions.items():
                dims.setdefault(s.arm, {}).setdefault(d, []).append(v)
            if "judge_exact" in s.extra:
                exact_vals.append(s.extra["judge_exact"])
            if "judge_kappa" in s.extra:
                kappa_vals.append(s.extra["judge_kappa"])
        dim_means = {arm: {d: round(mean(vs), 1) for d, vs in dd.items()}
                     for arm, dd in dims.items()}
        judge_agreement = {
            "exact": mean(exact_vals) if exact_vals else 0.0,
            "kappa": mean(kappa_vals) if kappa_vals else 0.0,
        }
        return Scorecard(benchmark=self.name, rows=scores,
                         meta={"judge": "gemini-3.1-pro+anthropic-crosscheck",
                               "dimensions": dim_means,
                               "dimension_source": "heuristic_title_match",
                               "dimension_note": (
                                   "Per-dimension means are a heuristic: criteria are "
                                   "bucketed by title-keyword match. BiomniBench-DA "
                                   "defines no dimensions; only the overall 0-100 score "
                                   "is benchmark-faithful."),
                               "dimension_read_note": (
                                   "READ PER-DIMENSION DELTAS AS DRIVING-CRITERION LEVELS, "
                                   "NOT percentages. At this n each dimension is typically "
                                   "driven by ONE rubric criterion, so a per-arm value maps "
                                   "to that criterion's A/B/C level (persisted per row in "
                                   "extra.judge_levels). For a PENALTY criterion "
                                   "(source_reliability is scored A=0 / B=-5 / C=-10) the "
                                   "displayed value is a satisfaction-normalization "
                                   "(A->100, B->50, C->0): a '50' means level B (one "
                                   "interpretive claim lacked inline source attribution), "
                                   "NOT 'half the sources are bad'. A single-trial "
                                   "per-dimension swing can be one criterion flipping one "
                                   "level on one task — never cite it as a powered result."),
                               "published_best": (
                                   "Claude Code+Opus 4.7 = 73.34 (paper figure: "
                                   "100-task mean with an Opus-4.7 agent; this run "
                                   "uses the 50 PUBLIC tasks with the Sonnet-4.6 "
                                   "default agent — a loose reference, not a "
                                   "head-to-head target)."),
                               "judge_note": (
                                   "Inter-judge agreement (kappa) below is Gemini-headline "
                                   "vs an Anthropic cross-check the paper found less "
                                   "calibrated (Opus-class linear kappa ~0.47); it is a "
                                   "confidence signal, NOT the paper's judge-vs-human "
                                   "kappa=0.70 (no human gold set exists here)."),
                               "contamination_control": (
                                   "Both arms receive an explicit 'work only from "
                                   "provided data; do not consult the source "
                                   "publication or answer key' directive (bare arm "
                                   "in-prompt; ECAA arm via PROMPT.md). LIMITATION: "
                                   "instruction-only — internet remains on for both "
                                   "arms, so this is symmetric defense-in-depth, not "
                                   "an egress guarantee."),
                               "penalty_divergence": (
                                   "The source-reliability penalty (A=0/B=-5/C=-10) "
                                   "is applied per the PAPER, but the dataset's "
                                   "shipped reference scorer's `Levels:` regex cannot "
                                   "read negative level values and silently drops "
                                   "them, so ECAA scores up to ~10 points BELOW the "
                                   "published 73.34 on any imperfect-sourcing run — a "
                                   "deliberate, paper-faithful divergence."),
                               "judge_agreement": judge_agreement})
