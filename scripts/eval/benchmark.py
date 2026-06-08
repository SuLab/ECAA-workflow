"""Core types + the Benchmark plugin contract.

`build_run` is the only arm-aware step; `collect`/`score`/`report` are
identical across arms so the ecaa-vs-direct delta isolates the wrapper.
"""
from __future__ import annotations
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path
from typing import Optional, Literal


class Arm(str, Enum):
    ECAA_WORKFLOW = "ecaa"            # compiler -> typed package -> harness+agent-claude.sh
    CLAUDE_CODE_DIRECT = "claude-direct"  # same Claude Code agent, bare instruction
    # E6: ECAA intake with the high-impact confirmation gate disabled, to isolate
    # the schema gate from the rest of the compiler. OFFLINE SCAFFOLDING: the
    # ungating itself is a TEST-ONLY server flag honored ONLY under ECAA_EVAL_LIVE
    # (see crates/server/src/chat_routes/eval_ungate.rs). NEVER a production mode.
    ECAA_UNGATED = "ecaa-ungated"


@dataclass
class Task:
    task_id: str
    prompt: str                       # natural-language description / question
    inputs: dict[str, Path]           # logical name -> staged input file
    rubric: Optional[dict]            # BiomniBench A/B/C rubric; None for Nekrutenko
    answer_key: Optional[Path]        # Nekrutenko canonical VCF dir; None for BiomniBench
    meta: dict = field(default_factory=dict)


@dataclass
class RunSpec:
    arm: Arm
    workdir: Path
    kind: Literal["ecaa_package", "bare"]
    instruction: str                  # bare-arm prompt; ecaa-arm intake text
    package_dir: Optional[Path] = None  # set for ecaa_package after intake
    session_id: Optional[str] = None    # chat session that emitted package_dir


@dataclass
class Output:
    trace_md: str
    answer_txt: str
    artifacts: dict[str, Path]        # e.g. {"vcf_dir": Path(...)}
    exit_ok: bool
    wall_secs: float


@dataclass
class Score:
    task_id: str
    arm: str
    trial: int
    overall: float                    # 0-100 (BBench) or 0-1 Jaccard*100 (Nekrut)
    dimensions: dict[str, float]      # BBench 6 dims; {} for Nekrut
    jaccard: Optional[float]          # Nekrut only
    error_cells: Optional[list[dict]] # Nekrut error-matrix cells; None for BBench
    judge_id: str                     # "gemini-3.1-pro" / "deterministic" / etc.
    # Free-form per-row metadata bucket (no fixed schema). Keys in use include
    # judge_cost_usd, cross_check, judge_exact/judge_kappa, partial_judging,
    # incomplete_reason, and claim_groundedness (the WS-3 narrative-grounding
    # visibility metric: {verified_count, total_claims, verified_pct,
    # reference_type}). Aggregators read keys defensively, never assume presence.
    extra: dict = field(default_factory=dict)


@dataclass
class Scorecard:
    benchmark: str
    rows: list[Score]
    meta: dict = field(default_factory=dict)


class Benchmark(ABC):
    @property
    @abstractmethod
    def name(self) -> str: ...

    @abstractmethod
    def fetch(self, cache_dir: Path) -> Path:
        """Ensure the pinned dataset is present; return its local root."""

    @abstractmethod
    def tasks(self, handle: Path, *, smoke: bool) -> list[Task]: ...

    @abstractmethod
    def build_run(self, task: Task, arm: Arm, workdir: Path) -> RunSpec: ...

    @abstractmethod
    def collect(self, spec: RunSpec, run_dir: Path) -> Output: ...

    @abstractmethod
    def score(self, task: Task, arm: Arm, output: Output, trial: int) -> Score: ...

    @abstractmethod
    def report(self, scores: list[Score]) -> Scorecard: ...

    def judge_requests(self, task: "Task", arm: "Arm", output: "Output") -> list[dict]:
        """Return judge requests for batch scoring.

        Each dict: {"role", "judge_id", "rubric", "trace", "answer"}.
        Default implementation returns an empty list; override in plugins that
        use the batch scoring path.
        """
        return []

    def locked_methods(self, task: "Task", arm: "Arm") -> list[tuple[str, str]]:
        """Return (stage_id, method) pairs to lock during chat-intake.

        For each pair the eval pre-sets the SME-named-method flag and names the
        method to the LLM so it is permitted to call `set_intake_method` —
        otherwise method-neutrality keeps the choice with the runtime agent.
        Default: lock nothing ("free" benchmarks). Recipe benchmarks override to
        pin the canonical tools for the ECAA arm only. Always [] for non-ECAA
        arms (the bare arm has no chat-intake to lock against).
        """
        return []

    def proposal_policy(self, task: "Task", arm: "Arm") -> str:
        """How chat-intake decides hypothesized-node proposals the LLM raises
        during intake (the v4 composer's gap-fill).

        The server refuses `propose_summary_confirmation`/`emit_package` while
        any proposal is undecided; a headless eval has no card to click, so the
        driver must approve or reject each one. "signoff" promotes the node into
        the DAG; "reject" declines it. Default "reject" — a benchmark must opt in
        to letting the ECAA arm expand its emitted DAG with unvetted nodes.
        """
        return "reject"

    def contamination_directive(self) -> "Optional[str]":
        """Optional package-wide anti-contamination instruction injected into BOTH
        arms (bare-arm prompt + ECAA `PROMPT.md`). Default None (opt-in): override
        in a plugin whose tasks are contamination-resistant (e.g. BiomniBench's
        held-out-style public set). Keeps the integrity control symmetric across
        arms; method choices are NOT named (tool neutrality)."""
        return None

    def assemble_score(self, task: "Task", arm: "Arm", output: "Output",
                       trial: int, verdicts: dict) -> "Score":
        """Build a Score from pre-fetched judge verdicts keyed by role.

        ``verdicts`` maps role name to the dict returned by ``judge_batch``
        for that request. Default falls back to synchronous ``score()``,
        ignoring ``verdicts``.
        """
        return self.score(task, arm, output, trial)

    def error_matrix(self, task: Task, arm: Arm, workdir: Path,
                     run_fn) -> Optional[list[dict]]:
        """Run the 36-cell PATH-shim fault-injection matrix and return per-cell
        classification dicts, or ``None`` if this benchmark has no error-matrix.

        ``run_fn(workdir, env)`` is injected by the caller so tests can pass a
        fake runner without spawning a real agent.  The default implementation
        returns ``None``; override in plugins that support fault injection
        (currently only Nekrutenko).
        """
        return None
