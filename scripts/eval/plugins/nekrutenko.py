"""Nekrutenko mtDNA variant-calling benchmark (deterministic, no judge).

ECAA arm: workflow description -> ecaa-workflow intake -> package -> harness.
Direct arm: problem statement + tool inventory only (Track-B equivalent).
Scored by per-sample VCF Jaccard + the 36-cell PATH-shim error matrix
(12 pattern×tool combinations × 3 seeds = 36 cells).
"""
from __future__ import annotations
import os
import tempfile
from pathlib import Path
from statistics import mean
from scripts.eval.benchmark import Arm, Benchmark, Output, RunSpec, Score, Scorecard
from scripts.eval.scoring.variant_overlap import flat_jaccard, _is_gvcf_path
from scripts.eval.scoring.error_matrix import classify_cell
from scripts.eval.services.datasets import scratch_root, stage_file

# Relative paths from the pinned nekrut/LLM-eval-paper repo (1175f72a…):
#   plan/PLAN.md          — default v2 implementation plan
#   data/raw/             — 4 paired-end .fq.gz samples + chrM.fa.gz
#   ground_truth/results/ — canonical .vcf.gz + collapsed.tsv answer key
# The fault-injection shim is eval-owned (scripts/eval/_eval_shim) and mounted
# into the agent container; it no longer ships from the dataset repo.
_PLAN = "plan/PLAN.md"
_SAMPLES = "data/raw"
_ANSWER_KEY = "ground_truth/results"

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


def _valid_vcf_count(root: Path) -> int:
    """Count VCFs under ``root`` with >=1 actual variant record (a non-blank,
    non-'#' line). 0-byte (silent_truncation) and header-only (wrong_format_output)
    outputs are NOT counted, so they can't be miscredited as a recovered sample."""
    import gzip
    n = 0
    for p in list(root.rglob("*.vcf")) + list(root.rglob("*.vcf.gz")):
        try:
            opener = gzip.open if p.suffix == ".gz" else open
            with opener(p, "rt") as fh:
                if any(ln.strip() and not ln.startswith("#") for ln in fh):
                    n += 1
        except OSError:
            pass
    return n


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
            stage_file(src, workdir / name)
        return RunSpec(arm, workdir, "bare", instr)

    def collect(self, spec, run_dir):
        # rglob pulls every VCF anywhere under the run dir, including cohort /
        # annotated / per-sample outputs in nested stage dirs. score() pools all
        # of them into one flat call set (dropping gVCF intermediates), so naming
        # and per-sample-vs-cohort organisation don't matter at scoring time.
        vcfs = {p.name: p for p in run_dir.rglob("*.vcf")}
        # Agents (and lofreq) routinely emit bgzipped .vcf.gz — the answer keys
        # themselves are .vcf.gz. Index those too, under both their own name and
        # the plain-.vcf stem. A plain .vcf already present wins (setdefault).
        for p in run_dir.rglob("*.vcf.gz"):
            vcfs.setdefault(p.name, p)
            vcfs.setdefault(p.name[:-3], p)
        return Output(trace_md="", answer_txt="", artifacts={"vcf_dir": run_dir,
                      "vcfs": vcfs}, exit_ok=True, wall_secs=0.0)  # exit/wall set by driver

    def score(self, task, arm, output, trial):
        # Recipe-agnostic CALL-SET overlap: the ECAA arm legitimately compiles a
        # germline GATK joint-genotyping workflow (per-sample gVCFs + a cohort
        # VCF), while the lofreq answer key is 4 per-sample VCFs. A per-sample
        # stem-match would score ~0 by construction on the naming mismatch even
        # when the same variants were called. Instead we pool ALL of the agent's
        # final-call VCFs (everything collect() indexed, minus gVCF
        # intermediates) and ALL answer-key VCFs into two flat variant sets and
        # take their Jaccard with the same ±0.02 AF tolerance.
        key_dir = task.answer_key
        ref_paths = sorted(Path(key_dir).glob("*.vcf.gz")) if key_dir else []
        # collect() indexes each VCF under one or more keys; de-dup by resolved
        # path so a file indexed under both name + stem isn't double-counted, and
        # drop gVCF intermediates (.g.vcf / .g.vcf.gz) — they are not final calls.
        seen: set = set()
        obs_paths: list[Path] = []
        for p in output.artifacts.get("vcfs", {}).values():
            rp = Path(p).resolve()
            if rp in seen or _is_gvcf_path(p):
                continue
            seen.add(rp)
            obs_paths.append(p)
        j = flat_jaccard(obs_paths, ref_paths) if ref_paths else 0.0
        return Score(task_id=task.task_id, arm=arm.value, trial=trial,
                     overall=round(j * 100.0, 2), dimensions={}, jaccard=j,
                     error_cells=output.artifacts.get("error_cells"),
                     judge_id="deterministic")

    def report(self, scores):
        # Aggregate error-matrix cells across ALL rows (every trial) per arm, so
        # multi-trial runs combine instead of the last trial overwriting. Cells
        # flagged ``inconclusive`` (the injected fault never reached the agent —
        # shim bypassed) are excluded from recover/diagnose rates and counted
        # separately, so a bypassed cell is never scored as a recovery.
        cells_by_arm: dict[str, list[dict]] = {}
        for row in scores:
            if row.error_cells:
                cells_by_arm.setdefault(row.arm, []).extend(row.error_cells)
        error_matrix: dict = {}
        for arm, cells in cells_by_arm.items():
            scored = [c for c in cells if not c.get("inconclusive")]
            by_pattern: dict = {}
            for cell in scored:
                pat = cell["pattern"]
                by_pattern.setdefault(pat, {"recover": [], "diagnose": []})
                by_pattern[pat]["recover"].append(cell["recover"])
                by_pattern[pat]["diagnose"].append(cell["diagnose"])
            error_matrix[arm] = {
                "recover_rate": mean(c["recover"] for c in scored) if scored else 0.0,
                "diagnose_rate": mean(c["diagnose"] for c in scored) if scored else 0.0,
                "n_cells": len(scored),
                "n_inconclusive": len(cells) - len(scored),
                "by_pattern": {
                    pat: {"recover_rate": mean(v["recover"]),
                          "diagnose_rate": mean(v["diagnose"])}
                    for pat, v in by_pattern.items()
                },
            }
        meta: dict = {"scorer": "variant_overlap_jaccard+error_matrix"}
        if error_matrix:
            meta["error_matrix"] = error_matrix
        return Scorecard(benchmark=self.name, rows=scores, meta=meta)

    def error_matrix_specs(self):
        """The 36 (pattern, tool, seed) cells of the PATH-shim fault matrix."""
        return [(pattern, tool, seed)
                for pattern, tool in _FAULT_PATTERNS
                for seed in _SEEDS]

    def run_error_cell(self, task, cell_spec, run_fn):
        """Run ONE PATH-shim fault cell and return its classification dict.

        Hands the arm a container-mountable fault-injection shim: sets the
        shim-contract env (``EVAL_INJECT_PATTERN``/``TARGET``/``STATE``) plus
        ``ECAA_EVAL_SHIM_DIR`` (the abs path to ``scripts/eval/_eval_shim``).
        The agent wrappers (agent-claude.sh / _bare_agent.sh) read
        ECAA_EVAL_SHIM_DIR to ro-mount the shim into the container, rw-mount the
        state dir, and PREPEND the shim dir to the container PATH so the real
        bwa/lofreq calls resolve to the shim FIRST — the fault crosses the
        container boundary instead of living only on the host.

        After ``run_fn(cell_dir, env)`` it classifies handle/recover/diagnose
        from the produced VCF count + failures.log, then runs BYPASS DETECTION:
        if the shim never wrote ``state_dir/invoked.<tool>`` the agent reached
        the real tool around the shim (absolute path / conda-activated bin /
        different tool), so the injected fault never landed — the cell is marked
        ``inconclusive`` (report() excludes it from recover/diagnose rates).
        ``shim_invoked`` is always recorded.

        The per-cell tempdir lives on scratch_root() (mounted disk), not /tmp,
        so parallel cells + per-cell package copies cannot fill the root
        filesystem; the state dir lives under it so the rw container mount lands
        on the same mounted disk."""
        pattern, tool, seed = cell_spec
        shim_dir = str(Path(__file__).resolve().parents[1] / "_eval_shim")

        with tempfile.TemporaryDirectory(dir=scratch_root()) as td:
            cell_dir = Path(td)
            state_dir = cell_dir / "_eval_state"
            state_dir.mkdir()

            env = os.environ.copy()
            env["EVAL_INJECT_PATTERN"] = pattern
            env["EVAL_INJECT_TARGET"] = tool
            env["EVAL_INJECT_STATE"] = str(state_dir)
            env["ECAA_EVAL_SHIM_DIR"] = shim_dir

            result = run_fn(cell_dir, env)

            produced_valid = _valid_vcf_count(cell_dir)
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
            cell = {"pattern": pattern, "tool": tool, "seed": seed, **classification}

            # Bypass detection: the shim records every invocation to
            # state_dir/invoked.<tool>. If it's absent the fault never reached
            # the agent's tool call, so this cell can't be scored as recovery.
            shim_invoked = (state_dir / f"invoked.{tool}").exists()
            cell["shim_invoked"] = shim_invoked
            if not shim_invoked:
                cell["inconclusive"] = True
            return cell

    def error_matrix(self, task, arm, workdir, run_fn):
        """Serial 36-cell sweep (kept for back-compat / non-parallel callers).
        The parallel driver schedules ``run_error_cell`` per spec instead."""
        return [self.run_error_cell(task, spec, run_fn)
                for spec in self.error_matrix_specs()]
