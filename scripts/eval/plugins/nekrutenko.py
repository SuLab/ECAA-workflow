"""Nekrutenko mtDNA variant-calling benchmark (deterministic, no judge).

ECAA arm: workflow description -> ecaa-workflow intake -> package -> harness.
Direct arm: problem statement + tool inventory only (Track-B equivalent).
Scored by per-sample VCF Jaccard + the 36-cell PATH-shim error matrix.
"""
from __future__ import annotations
import shutil
from pathlib import Path
from scripts.eval.benchmark import Arm, Benchmark, Output, RunSpec, Score, Scorecard
from scripts.eval.scoring.variant_overlap import mean_jaccard
from scripts.eval.scoring.error_matrix import classify_cell

# Relative paths confirmed in Step 1 (edit to match the probe):
_PLAN = "plans/v2.md"
_SAMPLES = "data"
_ANSWER_KEY = "ground_truth"
_SHIMS = "harness/shims"

_WORKFLOW_PROMPT = (
    "Call per-sample mitochondrial (chrM) variants for four paired-end "
    "Illumina samples: align with bwa, sort/index with samtools, call "
    "variants with lofreq, and write one VCF per sample, then build a "
    "collapsed per-variant table across samples."
)


class Nekrutenko(Benchmark):
    @property
    def name(self) -> str:
        return "nekrutenko"

    def fetch(self, cache_dir: Path) -> Path:
        from scripts.eval.services.datasets import load_lock, ensure
        lock = Path(__file__).resolve().parents[1] / "datasets.lock"
        return ensure(load_lock(lock)["nekrut/LLM-eval-paper"])

    def tasks(self, handle: Path, *, smoke: bool):
        from scripts.eval.benchmark import Task
        samples = {p.name: p for p in (handle / _SAMPLES).glob("*.fastq*")}
        return [Task(task_id="mtdna", prompt=_WORKFLOW_PROMPT, inputs=samples,
                     rubric=None, answer_key=handle / _ANSWER_KEY,
                     meta={"handle": str(handle)})]

    def build_run(self, task, arm, workdir):
        workdir.mkdir(parents=True, exist_ok=True)
        if arm == Arm.ECAA_WORKFLOW:
            return RunSpec(arm, workdir, "ecaa_package", task.prompt)
        # bare arm: problem statement + explicit tool inventory, no plan
        instr = task.prompt + "\n\nAvailable tools: bwa, samtools, lofreq, bcftools, awk."
        for name, src in task.inputs.items():
            shutil.copy(src, workdir / name)
        return RunSpec(arm, workdir, "bare", instr)

    def collect(self, spec, run_dir):
        vcfs = {p.name: p for p in run_dir.rglob("*.vcf")}
        return Output(trace_md="", answer_txt="", artifacts={"vcf_dir": run_dir,
                      "vcfs": vcfs}, exit_ok=True, wall_secs=0.0)  # exit/wall set by driver

    def score(self, task, arm, output, trial):
        key_dir = task.answer_key
        pairs = []
        for kvcf in sorted(Path(key_dir).glob("*.vcf")):
            obs = output.artifacts["vcfs"].get(kvcf.name)
            if obs:
                pairs.append((obs, kvcf))
        j = mean_jaccard(pairs) if pairs else 0.0
        return Score(task_id=task.task_id, arm=arm.value, trial=trial,
                     overall=round(j * 100.0, 2), dimensions={}, jaccard=j,
                     error_cells=output.artifacts.get("error_cells"),
                     judge_id="deterministic")

    def report(self, scores):
        return Scorecard(benchmark=self.name, rows=scores,
                         meta={"scorer": "variant_overlap_jaccard+error_matrix"})
