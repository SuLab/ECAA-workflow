"""Nekrutenko mtDNA variant-calling benchmark (deterministic, no judge).

ECAA arm: workflow description -> ecaa-workflow intake -> package -> harness.
Direct arm: problem statement + tool inventory only (Track-B equivalent).
Scored by the recipe-agnostic flat-pool VCF Jaccard (headline) PLUS the paper's
per-sample macro-mean M3 (companion), and the 36-cell PATH-shim error matrix
(12 pattern×tool combinations × 3 seeds = 36 cells), classified with the paper's
pattern-specific target_n recover metric + handle histogram + 3-signal diagnose.

SCOPE — what this harness reproduces vs the paper, stated explicitly so the
numbers are not over-read:
  * REPRODUCED: the error-injection matrix methodology (7 patterns, PATH-shim,
    seeds), the tolerant per-key Jaccard, and the recover/handle/diagnose scoring.
  * INTENTIONALLY OUT OF SCOPE (reproduces_plan_gradient = False): the paper's
    plan-granularity gradient (Track B / v0.5 / v1 / v1.25 / v1.5 / v2 /
    v2_defensive) and its recipe-implementer sweep (opus author + open-weight
    implementers such as qwen3.6:27b, plus the commodity-hardware / cost claims).
    This harness instead reuses Nekrutenko's methodology to measure the ECAA
    compiler-wrapper value-add (ECAA vs bare) on ONE model — a different
    experiment, not a reproduction of the paper's headline claims.
"""
from __future__ import annotations
import json
import os
import tempfile
from pathlib import Path
from statistics import mean
from scripts.eval.benchmark import Arm, Benchmark, Output, RunSpec, Score, Scorecard
from scripts.eval.scoring.variant_overlap import (flat_jaccard, _is_gvcf_path,
                                                  is_scratch_vcf,
                                                  macro_jaccard_by_sample)
from scripts.eval.scoring.error_matrix import classify_cell
from scripts.eval.services.datasets import scratch_root, stage_file

# Relative paths from the pinned nekrut/LLM-eval-paper repo (1175f72a…):
#   plan/PLAN.md          — the repo's v2 plan. RESERVED / NOT injected: this
#                           harness deliberately does NOT run the plan-granularity
#                           gradient (see the SCOPE note in the module docstring),
#                           so no plan text is fed to either arm. Kept only as a
#                           layout anchor (its existence is asserted by the path
#                           test) for a future gradient implementation.
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
# Error-matrix replication seeds. Env-overridable (ECAA_EVAL_NEK_SEEDS,
# comma-separated) so an operator can trade matrix replication for subscription
# cost without a code edit; default keeps the canonical 3 seeds (36 cells).
# e.g. ECAA_EVAL_NEK_SEEDS=42 -> 1 seed -> 12 cells/base run.
_SEEDS = tuple(
    int(s) for s in os.environ.get("ECAA_EVAL_NEK_SEEDS", "42,43,44").split(",")
    if s.strip()
)

# Canonical sample names (the 4 paired-end chrM samples), matching the Nekrutenko
# reference scorer's SAMPLES (score/score_run.py). Recover scoring is per-sample
# against these stems, so a stray extra VCF cannot inflate the count.
_SAMPLE_NAMES = ("M117-bl", "M117-ch", "M117C1-bl", "M117C1-ch")


def _target_n(pattern: str, n_samples: int = len(_SAMPLE_NAMES)) -> int:
    """Best-achievable valid-sample count for a fault pattern — a faithful port
    of the reference error_handling() (score/score_run.py):
    one_sample_fails -> n-1 (the injected sample can't pass);
    missing_lib_error / silent_truncation / wrong_format_output -> 0 (nothing
    should work; the best a run can do is detect-and-skip every sample);
    everything else (flake_first_call / slow_tool / stderr_warning_storm) -> n.
    Recover is then EXACT equality n_valid == target_n (NOT >=), so neither a
    stray extra VCF nor an undetected-corruption run is miscredited."""
    if pattern == "one_sample_fails":
        return n_samples - 1
    if pattern in ("missing_lib_error", "silent_truncation", "wrong_format_output"):
        return 0
    return n_samples


# Task prompt fed IDENTICALLY to both arms (ECAA intake + bare instruction).
# It names the canonical recipe tools (bwa/samtools/lofreq) deliberately — the
# eval pins that recipe (see locked_methods) for fidelity to the Nekrutenko
# reference. The objective states the SCIENTIFIC goal — detect the full variant
# spectrum INCLUDING low-frequency heteroplasmy — because the mtDNA answer key
# contains a ~4% heteroplasmic call (chrM:16455) that a "germline" (high-AF)
# framing would not target, leaving a prompt↔truth fidelity gap. This is a GOAL,
# not a method/parameter prescription: it must NOT name AF thresholds, lofreq
# flags (e.g. --no-default-filter), or trimming settings — doing so would hand
# the agent the methodology, defeat method-neutrality, and unfairly differ from
# what the bare arm has to derive on its own.
#
# The data location is also stated IN THE PROMPT (not via a post-emit directive):
# all prompting must go through chat intake so the eval tests realistic SME use.
# The reference + reads are staged into inputs/ for both arms (see build_run /
# _stage_inputs); naming inputs/ here lets the ECAA objective (surfaced into
# PROMPT.md) and the bare instruction both point the agent at the real data
# instead of synthesizing it.
_WORKFLOW_PROMPT = (
    "Perform per-sample germline variant calling on four paired-end "
    "Illumina mitochondrial (chrM) sequencing samples: align reads with "
    "bwa, sort and index with samtools, then run variant calling with "
    "lofreq to detect the full spectrum of short variants (SNVs and indels) "
    "in each sample — including low-frequency heteroplasmic variants, not "
    "only fixed/homoplasmic sites — writing one VCF per sample, and finally "
    "build a collapsed per-variant table across samples. The input FASTQ "
    "files and the chrM reference are provided in the inputs/ directory of "
    "this analysis; use those exact files as the data source — do not "
    "synthesize, simulate, or download substitute reads or references."
)


def _present_sample_count(root: Path) -> int:
    """Number of canonical samples (_SAMPLE_NAMES) with a present VCF under
    ``root``, counted via a DELIBERATE text-only heuristic: a VCF counts if it is
    non-empty with >=1 non-blank line. This is verdict-EQUIVALENT to the reference
    _samples_with_valid_vcf (score/score_run.py) on all 7 documented injection
    patterns — a header-only file (wrong_format_output) counts as present (the run
    failed to detect the corruption) and a 0-byte file (silent_truncation) does
    not — but it does NOT replicate the reference's structural gate, which also
    requires ``bcftools view -H`` to parse successfully (returncode 0). The two
    diverge only off the injection matrix: a corrupt, non-blank, non-VCF file
    (which never occurs on the 7 documented patterns, only hypothetically) would
    be over-counted as present here but rejected by the reference's bcftools
    parse. Per-sample (matched by stem substring) and capped at
    len(_SAMPLE_NAMES), so a stray extra VCF cannot inflate the recover count."""
    import gzip
    present: set = set()
    for p in list(root.rglob("*.vcf")) + list(root.rglob("*.vcf.gz")):
        sample = next((s for s in _SAMPLE_NAMES if s in p.name), None)
        if sample is None or sample in present:
            continue
        try:
            opener = gzip.open if p.suffix == ".gz" else open
            with opener(p, "rt") as fh:
                if any(ln.strip() for ln in fh):
                    present.add(sample)
        except OSError:
            pass
    return len(present)


# Narrative keys an ECAA per-task result.json uses for prose, mirroring
# scripts/eval/scoring/flatten.py::_RESULT_JSON_KEYS (same order).
_RESULT_NARRATIVE_KEYS = ("narrative", "interpretation", "summary",
                          "report", "answer", "text")


def _collect_result_summaries(cell_dir: Path) -> str:
    """Concatenate every per-task ECAA result.json narrative under
    ``runtime/outputs/<task_id>/result.json`` into one blob.

    classify_cell's diagnose scan only sees the harness's top-level stdout/stderr
    (exec_log). The ECAA arm reports per-task failures into structured per-task
    result.json files, NOT the top-level log, so its diagnose vocabulary
    (fail/skip/truncated + sample names) is invisible to classify_cell unless
    folded in — the bare arm prints the same failures straight to stdout. Merging
    these gives both arms identical diagnose vocabulary; classify_cell is
    unchanged. Best-effort: a missing/corrupt file contributes nothing."""
    outputs = Path(cell_dir) / "runtime" / "outputs"
    if not outputs.is_dir():
        return ""
    parts: list[str] = []
    for rj in sorted(outputs.glob("*/result.json")):
        try:
            data = json.loads(rj.read_text())
        except (OSError, ValueError):
            continue
        if not isinstance(data, dict):
            continue
        for key in _RESULT_NARRATIVE_KEYS:
            val = data.get(key)
            if isinstance(val, str) and val.strip():
                parts.append(val)
    return "\n".join(parts)


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
        # Stage the canonical chrM reference (rCRS) alongside the reads. The
        # ground-truth VCFs call variants RELATIVE to this reference, so both
        # arms must align to it (not a synthesized/consensus reference, which
        # would zero out every variant). data_acquisition runs network:none, so
        # the reference must be a provided input rather than a download.
        ref = handle / _SAMPLES / "chrM.fa.gz"
        if ref.exists():
            samples[ref.name] = ref
        return [Task(task_id="mtdna", prompt=_WORKFLOW_PROMPT, inputs=samples,
                     rubric=None, answer_key=handle / _ANSWER_KEY,
                     meta={"handle": str(handle)})]

    def build_run(self, task, arm, workdir):
        workdir.mkdir(parents=True, exist_ok=True)
        if arm == Arm.ECAA_WORKFLOW:
            return RunSpec(arm, workdir, "ecaa_package", task.prompt)
        # bare arm: problem statement + explicit tool inventory, no plan. Stage
        # the provided data into an inputs/ subdir (matching the ECAA arm's
        # pkg/inputs/ layout) so the shared prompt's "inputs/ directory" wording
        # is accurate for both arms — the bare agent finds the same files in the
        # same place the prompt names.
        instr = task.prompt + "\n\nAvailable tools: bwa, samtools, lofreq, bcftools, awk."
        inputs_dir = workdir / "inputs"
        inputs_dir.mkdir(parents=True, exist_ok=True)
        for name, src in task.inputs.items():
            stage_file(src, inputs_dir / name)
        return RunSpec(arm, workdir, "bare", instr)

    def locked_methods(self, task, arm):
        """Recipe eval: pin the paper's canonical tools (bwa + lofreq) on the
        ECAA arm so the compiled DAG matches the Nekrutenko reference recipe and
        the error-matrix injects against the same binaries the agent runs.
        `alignment` / `variant_calling` are the composer's bare discover axes
        for the variant_calling_germline archetype (sme-named strips any
        `discover_` prefix). The bare arm gets no chat-intake, so [] there."""
        if arm == Arm.ECAA_WORKFLOW:
            return [("alignment", "bwa"), ("variant_calling", "lofreq")]
        return []

    def proposal_policy(self, task, arm):
        """Recipe fidelity: reject any hypothesized gap-fill node so the emitted
        DAG stays the pinned bwa+lofreq reference recipe (the flat-pool VCF
        scorer needs only the per-sample calls, not added aggregation nodes)."""
        return "reject"

    def collect(self, spec, run_dir):
        # rglob pulls every VCF anywhere under the run dir, including cohort /
        # annotated / per-sample outputs in nested stage dirs. score() pools all
        # of them into one flat call set (dropping gVCF intermediates), so naming
        # and per-sample-vs-cohort organisation don't matter at scoring time.
        # EXCLUDE scratch/intermediate VCFs (e.g. an annotation step's
        # `tmp/<sample>.renamed.vcf` with the contig renamed for VEP): pooling
        # them — or letting the per-sample matcher pick one — penalizes a
        # multi-stage pipeline against the single-script bare arm (arm-unfair).
        # is_scratch_vcf is run-dir-relative so a /tmp-based run dir is fine.
        vcfs = {p.name: p for p in run_dir.rglob("*.vcf")
                if not is_scratch_vcf(p, run_dir)}
        # Agents (and lofreq) routinely emit bgzipped .vcf.gz — the answer keys
        # themselves are .vcf.gz. Index those too, under both their own name and
        # the plain-.vcf stem. A plain .vcf already present wins (setdefault).
        for p in run_dir.rglob("*.vcf.gz"):
            if is_scratch_vcf(p, run_dir):
                continue
            vcfs.setdefault(p.name, p)
            vcfs.setdefault(p.name[:-3], p)
        return Output(trace_md="", answer_txt="", artifacts={"vcf_dir": run_dir,
                      "vcfs": vcfs}, exit_ok=True, wall_secs=0.0)  # exit/wall set by driver

    def score(self, task, arm, output, trial):
        # HEADLINE = recipe-agnostic CALL-SET overlap. The ECAA arm is pinned to
        # lofreq (see locked_methods) and emits per-sample VCFs, but file NAMING
        # and per-sample-vs-cohort organisation are not guaranteed to match the
        # answer key's `{sample}.vcf.gz`. A strict per-sample stem-match would
        # score ~0 by construction on a naming mismatch even when the same
        # variants were called. So the headline pools ALL of the agent's
        # final-call VCFs (everything collect() indexed, minus gVCF intermediates)
        # and ALL answer-key VCFs into two flat sets and takes their Jaccard with
        # the same ±0.02 AF tolerance. We ALSO compute the paper's primary M3
        # (per-sample macro-mean) and surface it as a comparable companion metric.
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
        # Paper-comparable per-sample macro-mean (m3_jaccard): NOT the headline,
        # but reported so the run lines up with the paper's primary metric.
        macro, per_sample = (
            macro_jaccard_by_sample(obs_paths, key_dir, _SAMPLE_NAMES)
            if key_dir else (0.0, {}))
        return Score(task_id=task.task_id, arm=arm.value, trial=trial,
                     overall=round(j * 100.0, 2), dimensions={}, jaccard=j,
                     error_cells=output.artifacts.get("error_cells"),
                     judge_id="deterministic",
                     extra={"per_sample_macro_jaccard": macro,
                            "per_sample_jaccard": per_sample})

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
            # Handle-category histogram (recover/partial/propagate/crash) — the
            # paper's Table-7 four-tuple signature (e.g. 15/0/6/15 -> 21/15/0/0).
            handle_counts = {"recover": 0, "partial": 0, "propagate": 0, "crash": 0}
            for cell in scored:
                h = cell.get("handle")
                if h in handle_counts:
                    handle_counts[h] += 1
            # The flat recover_rate blends two scoring regimes — keyed by
            # _target_n, target_zero patterns (missing_lib_error / silent_truncation
            # / wrong_format_output, best = detect-and-skip all => 0 valid) score
            # recover differently from target_positive patterns (flake / slow /
            # warning / one_sample_fails, best = n or n-1 valid). Splitting them
            # keeps a high flat rate from masking a regime that systematically fails.
            zero_cells = [c for c in scored if _target_n(c["pattern"]) == 0]
            pos_cells = [c for c in scored if _target_n(c["pattern"]) != 0]
            recover_rate_by_target = {
                "target_zero": (mean(c["recover"] for c in zero_cells)
                                if zero_cells else None),
                "target_positive": (mean(c["recover"] for c in pos_cells)
                                    if pos_cells else None),
                "n_target_zero": len(zero_cells),
                "n_target_positive": len(pos_cells),
            }
            error_matrix[arm] = {
                "recover_rate": mean(c["recover"] for c in scored) if scored else 0.0,
                "recover_rate_label": (
                    "flat recover rate across all patterns; see "
                    "recover_rate_by_target for the target_zero vs "
                    "target_positive split"),
                "recover_rate_by_target": recover_rate_by_target,
                "diagnose_rate": mean(c["diagnose"] for c in scored) if scored else 0.0,
                "n_cells": len(scored),
                "n_inconclusive": len(cells) - len(scored),
                "handle_counts": handle_counts,
                "by_pattern": {
                    pat: {"recover_rate": mean(v["recover"]),
                          "diagnose_rate": mean(v["diagnose"])}
                    for pat, v in by_pattern.items()
                },
            }
        meta: dict = {"scorer": "variant_overlap_jaccard+error_matrix"}
        if error_matrix:
            meta["error_matrix"] = error_matrix
        # Per-arm mean of the paper-comparable per-sample macro Jaccard (M3),
        # alongside the recipe-agnostic flat-pool headline (Score.overall). Both
        # surfaced so the headline stays naming-agnostic while the per-sample
        # number lines up with the paper's primary metric.
        per_sample_by_arm: dict[str, list[float]] = {}
        for row in scores:
            v = (row.extra or {}).get("per_sample_macro_jaccard")
            if v is not None:
                per_sample_by_arm.setdefault(row.arm, []).append(v)
        if per_sample_by_arm:
            meta["per_sample_macro_jaccard"] = {
                arm: round(mean(vs), 4) for arm, vs in per_sample_by_arm.items()
            }
            meta["jaccard_note"] = (
                "Headline (overall) is the recipe-agnostic FLAT-POOL Jaccard; "
                "per_sample_macro_jaccard is the paper's primary M3 (per-sample "
                "macro-mean). They differ when calls move between per-sample and "
                "pooled organisation — read the per-sample number for paper parity.")
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
        from the produced VCF count + failures.log. Before classifying, the ECAA
        arm's per-task ``runtime/outputs/<task_id>/result.json`` narratives are
        folded into the exec_log (see _collect_result_summaries) so the diagnose
        scan sees the same fail-word vocabulary the bare arm prints to stdout —
        keeping the arm-fairness contract. It then runs BYPASS DETECTION:
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

            produced_valid = _present_sample_count(cell_dir)
            # failures.log may be written anywhere under the run dir (results/,
            # cwd, ...); concatenate every one found, mirroring the reference's
            # results/failures.log read.
            failures_log = "\n".join(
                p.read_text(errors="replace")
                for p in cell_dir.rglob("failures.log") if p.is_file())
            # The agent's captured stdout/stderr IS the reference's exec.log —
            # scanned for the "N/M samples" summary line + a sample/fail-word
            # mention (the two diagnose signals beyond failures.log). Populated
            # only when the runner captured it (cells run run_ecaa_package with
            # capture=True; run_bare always captures).
            exec_log = getattr(result, "stdout", "") or ""
            # Fold ECAA's per-task result.json narratives into the exec_log so
            # classify_cell's diagnose scan sees the same fail-word vocabulary
            # across arms (the bare arm prints failures to stdout; the ECAA arm
            # reports them into per-task result.json). classify_cell itself is
            # unchanged. The bare arm has no runtime/outputs/ tree, so this is a
            # no-op there and the arm-fairness contract is preserved.
            summaries = _collect_result_summaries(cell_dir)
            if summaries:
                exec_log = (exec_log + "\n" + summaries) if exec_log else summaries
            classification = classify_cell(
                exit_code=0 if result.exit_ok else 1,
                failures_log=failures_log,
                produced_valid=produced_valid,
                target_n=_target_n(pattern),
                n_samples=len(_SAMPLE_NAMES),
                exec_log=exec_log,
                samples=_SAMPLE_NAMES,
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
