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
