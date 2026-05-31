"""BiomniBench-DA (process-level, LLM-judged) — 50 public tasks.

ECAA arm: question -> ecaa-workflow intake -> package -> harness; flatten
runtime outputs to trace.md+answer.txt. Direct arm: bare Claude Code on the
question. Scored by Gemini 3.1 Pro (headline) + Anthropic (cross-check).
"""
from __future__ import annotations
import json
import shutil
from pathlib import Path
from statistics import mean
from scripts.eval.benchmark import Arm, Benchmark, Output, RunSpec, Score, Scorecard, Task
from scripts.eval.scoring.flatten import flatten_outputs
from scripts.eval.services.judge import judge

# HF field names confirmed in Step 1 (edit to match the probe):
_F_QUESTION = "question"
_F_RUBRIC = "rubric"
_F_DATA = "data_files"

_OUTPUT_CONTRACT = (
    "\n\nWrite your analytical narrative (decisions, intermediate findings, "
    "interpretation) to trace.md and a short structured answer to answer.txt."
)


class BiomniBench(Benchmark):
    @property
    def name(self) -> str:
        return "biomnibench"

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
        # Adjust per probe: tasks may be one json/jsonl, or per-task dirs.
        jsonl = next(handle.rglob("*.jsonl"), None)
        if jsonl:
            return [json.loads(l) for l in jsonl.read_text().splitlines() if l.strip()]
        return [json.loads(p.read_text()) for p in sorted(handle.rglob("task*.json"))]

    def _to_task(self, handle: Path, r: dict) -> Task:
        tid = r.get("id") or r.get("task_id")
        inputs = {Path(f).name: handle / f for f in r.get(_F_DATA, [])}
        return Task(task_id=str(tid), prompt=r[_F_QUESTION], inputs=inputs,
                    rubric=r[_F_RUBRIC], answer_key=None, meta={})

    def build_run(self, task, arm, workdir):
        workdir.mkdir(parents=True, exist_ok=True)
        if arm == Arm.ECAA_WORKFLOW:
            return RunSpec(arm, workdir, "ecaa_package", task.prompt + _OUTPUT_CONTRACT)
        for name, src in task.inputs.items():
            if src.exists():
                shutil.copy(src, workdir / name)
        return RunSpec(arm, workdir, "bare", task.prompt + _OUTPUT_CONTRACT)

    def collect(self, spec, run_dir):
        if spec.kind == "ecaa_package":
            trace, answer = flatten_outputs(run_dir / "runtime" / "outputs",
                                            run_dir / "WORKFLOW.json")
        else:
            trace = (run_dir / "trace.md").read_text() if (run_dir / "trace.md").exists() else ""
            answer = (run_dir / "answer.txt").read_text() if (run_dir / "answer.txt").exists() else ""
        return Output(trace_md=trace, answer_txt=answer, artifacts={},
                      exit_ok=True, wall_secs=0.0)

    def score(self, task, arm, output, trial):
        headline = judge("gemini-3.1-pro", task.rubric, output.trace_md, output.answer_txt)
        cross = judge("anthropic-opus", task.rubric, output.trace_md, output.answer_txt)
        return Score(task_id=task.task_id, arm=arm.value, trial=trial,
                     overall=headline["overall"], dimensions=headline["dimensions"],
                     jaccard=None, error_cells=None, judge_id="gemini-3.1-pro",
                     extra={"cross_check": cross["overall"],
                            "cross_dimensions": cross["dimensions"]})

    def report(self, scores):
        dims: dict[str, dict[str, list[float]]] = {}
        for s in scores:
            for d, v in s.dimensions.items():
                dims.setdefault(s.arm, {}).setdefault(d, []).append(v)
        dim_means = {arm: {d: round(mean(vs), 1) for d, vs in dd.items()}
                     for arm, dd in dims.items()}
        return Scorecard(benchmark=self.name, rows=scores,
                         meta={"judge": "gemini-3.1-pro+anthropic-crosscheck",
                               "dimensions": dim_means,
                               "published_best": "Claude Code+Opus 4.7 = 73.34"})
