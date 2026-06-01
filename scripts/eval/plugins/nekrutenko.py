"""Nekrutenko mtDNA variant-calling benchmark (deterministic, no judge).

ECAA arm: workflow description -> ecaa-workflow intake -> package -> harness.
Direct arm: problem statement + tool inventory only (Track-B equivalent).
Scored by per-sample VCF Jaccard + the 36-cell PATH-shim error matrix
(12 pattern×tool combinations × 3 seeds = 36 cells).
"""
from __future__ import annotations
import os
import shutil
import tempfile
from pathlib import Path
from statistics import mean
from scripts.eval.benchmark import Arm, Benchmark, Output, RunSpec, Score, Scorecard
from scripts.eval.scoring.variant_overlap import mean_jaccard
from scripts.eval.scoring.error_matrix import classify_cell

# Relative paths from the pinned nekrut/LLM-eval-paper repo (1175f72a…):
#   plan/PLAN.md          — default v2 implementation plan
#   data/raw/             — 4 paired-end .fq.gz samples + chrM.fa.gz
#   ground_truth/results/ — canonical .vcf.gz + collapsed.tsv answer key
#   harness/error_shims/  — flat dir: bwa (script), lofreq (script), shim.py
_PLAN = "plan/PLAN.md"
_SAMPLES = "data/raw"
_ANSWER_KEY = "ground_truth/results"
_SHIMS = "harness/error_shims"

# 12 (pattern, tool) combinations from the Nekrutenko paper (Table 5).
# 5 patterns target both bwa and lofreq (10 cells); 2 are lofreq-only (2 cells).
# Combined with _SEEDS this yields 12 × 3 = 36 matrix cells.
_FAULT_PATTERNS = [
    ("flake_first_call",     "bwa"),
    ("flake_first_call",     "lofreq"),
    ("one_sample_fails",     "bwa"),
    ("one_sample_fails",     "lofreq"),
    ("slow_tool",            "bwa"),
    ("slow_tool",            "lofreq"),
    ("stderr_warning_storm", "bwa"),
    ("stderr_warning_storm", "lofreq"),
    ("missing_lib_error",    "bwa"),
    ("missing_lib_error",    "lofreq"),
    ("silent_truncation",    "lofreq"),
    ("wrong_format_output",  "lofreq"),
]
_SEEDS = (42, 43, 44)

_WORKFLOW_PROMPT = (
    "Perform per-sample germline variant calling on four paired-end "
    "Illumina mitochondrial (chrM) sequencing samples: align reads with "
    "bwa, sort and index with samtools, then run variant calling with "
    "lofreq to detect short variants (SNPs and indels), writing one VCF "
    "per sample, and finally build a collapsed per-variant table across "
    "samples."
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
        samples = {p.name: p for p in (handle / _SAMPLES).glob("*.fq.gz")}
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
        # Canonical answer-key VCFs are bgzip-compressed (.vcf.gz); agents may
        # produce plain .vcf outputs.  Match by stem (strip .gz if present) so
        # M117-bl.vcf.gz pairs with the agent's M117-bl.vcf.
        for kvcf in sorted(Path(key_dir).glob("*.vcf.gz")):
            stem = kvcf.name[:-3] if kvcf.name.endswith(".gz") else kvcf.name
            obs = output.artifacts["vcfs"].get(stem) or output.artifacts["vcfs"].get(kvcf.name)
            if obs:
                pairs.append((obs, kvcf))
        j = mean_jaccard(pairs) if pairs else 0.0
        return Score(task_id=task.task_id, arm=arm.value, trial=trial,
                     overall=round(j * 100.0, 2), dimensions={}, jaccard=j,
                     error_cells=output.artifacts.get("error_cells"),
                     judge_id="deterministic")

    def report(self, scores):
        error_matrix: dict = {}
        for row in scores:
            cells = row.error_cells
            if not cells:
                continue
            arm = row.arm
            arm_entry = error_matrix.setdefault(arm, {
                "recover_rate": 0.0,
                "diagnose_rate": 0.0,
                "n_cells": 0,
                "by_pattern": {},
            })
            arm_entry["recover_rate"] = mean(c["recover"] for c in cells)
            arm_entry["diagnose_rate"] = mean(c["diagnose"] for c in cells)
            arm_entry["n_cells"] = len(cells)
            by_pattern: dict = {}
            for cell in cells:
                pat = cell["pattern"]
                by_pattern.setdefault(pat, {"recover": [], "diagnose": []})
                by_pattern[pat]["recover"].append(cell["recover"])
                by_pattern[pat]["diagnose"].append(cell["diagnose"])
            arm_entry["by_pattern"] = {
                pat: {
                    "recover_rate": mean(v["recover"]),
                    "diagnose_rate": mean(v["diagnose"]),
                }
                for pat, v in by_pattern.items()
            }
        meta: dict = {"scorer": "variant_overlap_jaccard+error_matrix"}
        if error_matrix:
            meta["error_matrix"] = error_matrix
        return Scorecard(benchmark=self.name, rows=scores, meta=meta)

    def error_matrix(self, task, arm, workdir, run_fn):
        """Run the 36-cell PATH-shim fault-injection sweep.

        For each of the 12 pattern×tool combinations × 3 seeds (36 cells total),
        prepare a per-cell ``_eval_bin/`` directory that symlinks the real shim
        wrapper scripts (``bwa`` and ``lofreq``) from ``harness/error_shims/``
        into a per-run bin dir, then prepend that dir onto PATH.  The shim
        contract env vars mirror run_one.py's ``setup_inject()``:

          EVAL_INJECT_PATTERN  — which failure mode to inject
          EVAL_INJECT_TARGET   — which tool the pattern applies to
          EVAL_INJECT_STATE    — per-run state dir for cross-call counters
          EVAL_REAL_BIN_DIR    — where the real (non-shimmed) binaries live

        There is no EVAL_INJECT_SEED env var; seed isolation is achieved by
        using a fresh EVAL_INJECT_STATE directory for each cell.

        ``run_fn`` is injected so offline tests can pass a fake without spawning
        a real agent.  The live driver passes a closure that re-runs the arm via
        agent_runner.
        """
        handle_dir = Path(task.meta.get("handle", ""))
        shims_root = handle_dir / _SHIMS if handle_dir.is_dir() else None

        cells = []
        for pattern, tool in _FAULT_PATTERNS:
            for seed in _SEEDS:
                with tempfile.TemporaryDirectory() as td:
                    cell_dir = Path(td)

                    # Per-cell state dir isolates cross-call counters between seeds.
                    state_dir = cell_dir / "_eval_state"
                    state_dir.mkdir()

                    # Build a bin dir with symlinks to the real shim wrapper scripts
                    # (bwa and lofreq).  In offline tests shims_root is None and we
                    # create placeholder paths; the fake run_fn never invokes them.
                    bin_dir = cell_dir / "_eval_bin"
                    bin_dir.mkdir()
                    if shims_root is not None:
                        for shimmed_tool in ("bwa", "lofreq"):
                            link = bin_dir / shimmed_tool
                            link.symlink_to(shims_root / shimmed_tool)

                    # Build an env with the fault shim dir prepended to PATH
                    # and the shim contract vars set so shims know what to inject.
                    env = os.environ.copy()
                    env["PATH"] = str(bin_dir) + os.pathsep + env.get("PATH", "")
                    env["EVAL_INJECT_PATTERN"] = pattern
                    env["EVAL_INJECT_TARGET"] = tool
                    env["EVAL_INJECT_STATE"] = str(state_dir)

                    result = run_fn(cell_dir, env)

                    # Count produced VCFs in the cell workdir.
                    produced_valid = len(list(cell_dir.rglob("*.vcf")))

                    # Read failures.log if the agent wrote one.
                    failures_log = ""
                    log_path = cell_dir / "failures.log"
                    if log_path.exists():
                        failures_log = log_path.read_text()

                    classification = classify_cell(
                        exit_code=0 if result.exit_ok else 1,
                        failures_log=failures_log,
                        produced_valid=produced_valid,
                        expected_valid=4,
                    )
                    cells.append({
                        "pattern": pattern,
                        "tool": tool,
                        "seed": seed,
                        **classification,
                    })
        return cells
