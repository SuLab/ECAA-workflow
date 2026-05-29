# Agent Instructions

You are executing one harness-dispatched task from a computational biology workflow defined in WORKFLOW.json.
**Workflow:** Shotgun metagenomics or 16S amplicon taxonomic profiling. Standard
pipeline: raw QC, trim, classify reads against a reference database
(Kraken2 / MetaPhlAn / QIIME2-DADA2), then alpha + beta diversity
with group-comparison statistics. Mirrors today's
`config/modalities/metagenomics.yaml` + `config/archetypes/`.


## Dispatch Contract
1. Read `ECAA_TASK_ID`. That is the only task you may execute in this invocation.
2. Read WORKFLOW.json only to inspect that task's spec and completed dependency outputs.
3. Write outputs only under `runtime/outputs/$ECAA_TASK_ID/`.
4. Write the state transition only to `runtime/outputs/$ECAA_TASK_ID/state.patch.json`.
5. Include top-level `harness_run_id` and `dispatch_epoch` values copied from `ECAA_HARNESS_RUN_ID` and `ECAA_DISPATCH_EPOCH` in that patch.
6. Do not edit WORKFLOW.json. The harness is the only writer of task state.
7. Do not execute any other ready task. The harness will invoke a new agent for the next dispatch.
8. Append a JSON line to runtime/LOG.jsonl for audit context only.

## Current state
- Completed: 0
- Ready: 0
- Blocked: 0
- Pending: 16

## Rules
- Execute only the task named by `ECAA_TASK_ID`
- Never skip a task's dependencies
- Never mark, patch, or edit any task other than `ECAA_TASK_ID`
- For discovery tasks, consult the policy file referenced in the task spec
- For blocked tasks, write a clear reason and what you tried
- All decisions go in runtime/LOG.jsonl as one JSON object per line
- DO NOT WRITE TO runtime/decisions.jsonl. That file is owned by the
conversation/server layer and holds only the typed DecisionRecord
taxonomy (kinds: confirm | reject | unblock | branch | emit_package |
amend_stage | rerun_task | select_sensitivity_winner |
cross_version_diff | post_hoc_deviation | auto_advanced |
applied_structured_decision | disposition_proposed |
disposition_applied | disposition_rejected | undone_amendment |
budget_changed | user_note). Free-form audit entries with `kind` at
the top level break the typed schema and are filtered out by the
server's /decisions endpoint. Use runtime/LOG.jsonl for your own
audit and stage-decision artifacts (decision.json + blocker.json +
sme-selection.json under runtime/outputs/<task_id>/) for the
per-stage records.

## Discovery procedure: env capability + spec-preferred methods (Phases 2 + 3)

Before composite-scoring any `discover_*` candidate pool:

1. **Read `runtime/env_capability.json`** (harness-written at startup). For each candidate method, check required capabilities against the report. Candidates whose required capability is unavailable get tagged `{ env_capability_skip: true, missing: [cap...] }` in `decision.json::candidate_pool_full` and are excluded from composite scoring. This prevents silent Python-analog substitution when a spec pins an R/SCENIC/lisi tool that isn't installed.

2. **Apply `task.spec.spec_preferred_methods` boosts.** When the stage's task spec carries a non-empty `spec_preferred_methods: {method_id: rationale}` map, apply a `+0.30` boost on the `spec_match` composite axis to every candidate whose `method_id` is a key in that map. Record the boost in `decision.json::candidate_pool_full[i].spec_match_applied` + cite the rationale. Spec-preferred candidates that are env-available MUST outrank non-spec candidates of otherwise equal score. Set `decision.json::spec_preference_applied = true` when the final pick was re-ranked by the boost.

Together these rules mean: spec-preferred tools that ARE in the env get picked automatically; spec-preferred tools that AREN'T fall through cleanly with a structured `env_capability_skip` rationale rather than silently swapping to a Python analog.

## SME-supplied data inputs (consult BEFORE public-repo discovery)

When the SME has registered local data (file present at `runtime/inputs.json`, also surfaced under the `## SME-supplied data inputs` section of this CONTEXT.md), the `data_acquisition` stage MUST consume those registrations as its primary input and SHOULD NOT propose public-repository fetchers as the top candidate.

1. **At the start of `discover_data_acquisition`**, check `runtime/inputs.json`. The schema is `[{ input_id, label, kind: "local_path" | "uploaded_files", root_path, files: [{relpath, size_bytes, sha256}], registered_at, registered_by }, ...]`.
2. **If the array is non-empty**, your candidate pool MUST include `sme_supplied_local_path` (when any input has `kind: "local_path"`) and/or `sme_supplied_uploaded_files` (for `kind: "uploaded_files"`). Score these candidates with a strong spec-preference boost (`+0.40` on `spec_match`) so they outrank generic GEO/SRA fetchers. Auto-pick when the boost yields a clear winner; only block for SME approval when there's a genuine ambiguity (e.g. mixed local + cited-but-unsupplied accessions).
3. **The `data_acquisition` task itself** (the compute task that follows discovery) reads `runtime/inputs.json` and copies / symlinks the SME files into the canonical layout `runtime/outputs/data_acquisition/data/<source_label>/<filename>`. Compute and verify each file's sha256 against the manifest; flag mismatches as a blocker (data drift between registration and execution).
4. **Empty `runtime/inputs.json`** (or the file absent entirely) means the SME relies on public accessions captured in CONTEXT.md prose — the existing public-repo dispatcher path applies unchanged.

This rule is independent of the env_capability and spec_preferred_methods rules above and runs FIRST: an SME-registered input always takes precedence over any other ranking signal.

## Empty-completion is NOT permitted

When you apply the available SME decisions and still cannot produce non-empty output (e.g. header-only tables, all-zero counts, every compartment failing a minimum-samples gate, any sentinel like `overall_<stage>_not_run: true`), you MUST re-block the task rather than mark it `completed` with an empty result. Write a new `blocker.json` with narrower `decision_points_for_sme` (for example: 'sample-level age TSV required', 'pick a different threshold', 'alternative grouping variable'), set `task.state.status = "blocked"`, and stop. Do not silently advance the DAG past an empty computation.

The harness + validator enforce this:
- Any completed task whose result carries an `overall_*_not_run: true` key is automatically re-blocked by the harness on the next iteration.
- Any completed compute task whose output tables (listed under `manifest.downstream_handoff` or the stage's canonical layout) contain zero data rows is flagged PASS-WITH-WARN by its validator.

## Figures (REQUIRED when the task spec has `required_figures`)

Every compute task whose `spec.required_figures` is non-empty MUST produce each listed figure under `runtime/outputs/<task_id>/figures/<figure_id>.png` and `<figure_id>.pdf` **before** marking the task `completed`. Use the shared plotting library shipped with this package. If `task.spec.plot_stage_id` is present, pass that value as `stage_id`; otherwise pass the task id:

```python
import sys
from pathlib import Path
sys.path.insert(0, str(Path.cwd()))  # so `runtime.plotting` resolves
from runtime.plotting.core import generate
mf = generate(
stage_id=task_spec.get("plot_stage_id", "<task_id>"),
outputs_dir=Path("runtime/outputs/<task_id>"),
required=<task.spec.required_figures>,
)
# mf.written is a dict of figure_id -> path; include it in task.state.result
```

The harness treats missing required figure PNG/PDF files and a missing `figures/manifest.json` as a hard completion failure. If input artifacts are absent, block the task with a concrete missing-input reason instead of completing with skipped required figures. Do NOT silently omit figures from the result.

The library owns determinism (Agg backend, stripped metadata, seeded RNG, theme baseline from `runtime/plotting/theme.json`). Output is dual-format: a 300dpi PNG and a vector PDF for every figure. Do NOT import matplotlib directly — go through `runtime.plotting.core` helpers (`violin`, `bar`, `scatter`, `volcano`, `heatmap`) plus `categorical_palette(n)` for any categorical encoding (Wong/Glasbey colorblind-safe; never `tab10`/`tab20`) so every figure across the package is byte-reproducible.

**For R-based tasks** (Seurat / DESeq2 / Bioconductor), source the parallel R-side library at `runtime/plotting_r/core.R` and call `ecaa_savefig(plot, path, stage_id=...)`. Both renderers consume the same `theme.json`, the same Wong palette, and produce figures at the same figure_id catalog so the validator's `figures_present` check is renderer-agnostic.

## Hardware-aware execution

You run under a harness that passes a per-task hardware envelope in environment variables (prefix `ECAA_HW_`). Never ignore these vars. Parse `ECAA_HW_TOOL_THREAD_CURVES`, `ECAA_HW_ENV_OVERRIDES`, `ECAA_HW_INTAKE_FACTS`, `ECAA_HW_CONCURRENT_PEERS_BY_CLASS` as JSON; the rest are plain scalars.

- BLAS / OpenMP / NumExpr / etc. thread budgets are ALREADY set as bare env vars on your shell environment by the harness (`OMP_NUM_THREADS`, `OPENBLAS_NUM_THREADS`, `MKL_NUM_THREADS`, `NUMEXPR_NUM_THREADS`, `BLIS_NUM_THREADS`, `TBB_NUM_THREADS`, `RAYON_NUM_THREADS`, `NUMBA_NUM_THREADS`, `JULIA_NUM_THREADS`, `POLARS_MAX_THREADS`, `VECLIB_MAXIMUM_THREADS`, `GOTO_NUM_THREADS`). Numerical libraries read these at .so init time, so DO NOT use `Sys.setenv()` in R or `os.environ[...] = ...` in Python to set them — that runs after BLAS has already loaded and is a no-op. To change BLAS thread count at runtime use `RhpcBLASctl::blas_set_num_threads(N)` in R or `threadpoolctl.threadpool_limits(N)` in Python. The bundled `ECAA_HW_ENV_OVERRIDES` JSON is back-compat metadata; you do not need to parse or re-export it.
- Pass `--threads N` (or the tool-specific equivalent) equal to `min(ECAA_HW_RECOMMENDED_THREADS, ECAA_HW_TOOL_THREAD_CURVES[<your-tool>])`. Never default to 1, never default to `$(nproc)`. If your tool isn't in `tool_thread_curves`, fall back to `ECAA_HW_RECOMMENDED_THREADS`.
- Piped multi-threaded tools (`bwa mem -t X | samtools sort -@ Y`): split `ECAA_HW_RECOMMENDED_THREADS` favoring the CPU-bound stage. Typical split: `X = 0.6 * recommended_threads`, `Y = 0.4 * recommended_threads`.
- Distinguish compression/decompression thread flags (`samtools -@`, `pigz -p`, `bgzip -@`) from compute thread flags (`--threads`). They are separate pools and should not share a budget.
- GPU routing (not recommendation): if the chosen method has an entry in `policies/gpu-capability-policy.json` AND `ECAA_HW_GPU != "none"` AND the `requires` binaries are on `PATH` (probe with `which`), invoke the `gpu_impl`. This is routing — the method was selected upstream. On missing binary, fall back to `cpu_impl` with a warning logged to `runtime/task-log.jsonl`.
- Size batch parameters (AlphaFold tile, Parabricks batch, ESMfold max sequence length) to the VRAM implied by `ECAA_HW_GPU` (format `nvidia-<kind>:<count>`; VRAM is implicit from kind).
- Multi-phase tools (DeepVariant `make_examples → call_variants → postprocess_variants`): read `phase_thread_counts` from `policies/compute-resource-policy.json` rather than using a single `recommended_threads` for every phase.
- Respect `ECAA_HW_CONCURRENT_PEERS_BY_CLASS`. When your class's peer count > 1 in that map, reduce your thread budget proportionally — the scheduler has granted you a slice, not the whole box. This field is always `{cpu_heavy: 1}` today but will be dynamic once parallel scheduling lands.

## Auto-detect compute and fan out embarrassingly parallel work

You are expected to fully utilize the compute granted to you. Detect what's actually available at runtime and fan out independent units of work across all of it — never default to a serial loop when the work is parallelizable.

### Step 1 — Detect at runtime, don't trust prior assumptions

Run these probes before any heavy work and log the results to `runtime/outputs/<task_id>/progress.log`:

- **Total cores**: `nproc --all` (Linux) — fallback `getconf _NPROCESSORS_ONLN` if `nproc` is missing. In Python: `os.cpu_count()`. In R: `parallel::detectCores(logical=TRUE)`.
- **Free memory (MiB)**: `free -m | awk 'NR==2 {print $7}'` (the "available" column on Linux). In Python: `psutil.virtual_memory().available // (1024*1024)`. In R: read `/proc/meminfo` `MemAvailable`.
- **GPU presence**: `nvidia-smi --query-gpu=name,memory.free --format=csv,noheader 2>/dev/null` — empty output = no GPU. Cross-check against `ECAA_HW_GPU`.
- **Container limits**: if running under cgroups v2, also check `/sys/fs/cgroup/cpu.max` and `/sys/fs/cgroup/memory.max` — these can be tighter than the host nproc.

### Step 2 — Compute the worker pool

- **Effective core budget**: `cores = min(detected_cores, ECAA_HW_RECOMMENDED_THREADS or detected_cores)`. The env var is a ceiling, not a target. If unset, use the full detected count.
- **Reserve 1 core** for the orchestrator process: `usable = max(1, cores - 1)`.
- **Inner thread budget per unit**: pick from `ECAA_HW_TOOL_THREAD_CURVES[your-tool]` if your tool is listed; otherwise default to `min(4, usable)` for BLAS-heavy R/Python (SCTransform, DESeq2, Seurat anchor finding) or `1` for pure-Python single-threaded code.
- **Outer worker count**: `outer_workers = max(1, floor(usable / inner_threads_per_unit))`. Total active threads stay bounded: `outer_workers * inner_threads_per_unit ≤ usable`.
- **Memory check**: estimate per-worker memory (e.g. an SCTransform on a 30k-cell Seurat object ≈ 6 GiB). If `outer_workers * per_worker_gib > available_gib`, reduce `outer_workers` until it fits, OR switch to BPCells/DelayedArray on-disk backing per the memory-discipline policy.

### Step 3 — Fan out

- **R**: `BiocParallel::bplapply(units, FUN, BPPARAM = MulticoreParam(workers = outer_workers))` on Linux; never `SnowParam` (slower fork-then-PSOCK overhead). The harness sets BLAS env vars to `recommended_threads` for the parent process — that's correct for a single-process Rscript, but oversubscribes when you fan out (each forked worker inherits `OMP_NUM_THREADS=recommended_threads` and the BLAS pool has already been created). To prevent oversubscription, call `RhpcBLASctl::blas_set_num_threads(inner_threads_per_unit)` INSIDE each worker (the FUN argument), AFTER `mclapply` forks. `Sys.setenv()` BEFORE the fan-out does NOT work — BLAS reads its thread count at .so init, which already happened when R started.
- **Python**: `concurrent.futures.ProcessPoolExecutor(max_workers = outer_workers)` for CPU-bound work, or `joblib.Parallel(n_jobs = outer_workers, backend = "loky")`. Each child inherits the harness-set env vars; constrain per-worker BLAS at runtime with `threadpoolctl.threadpool_limits(inner_threads_per_unit)` inside the worker function (set, don't override the env vars).
- **Shell**: `parallel -j outer_workers ...` for independent CLI invocations.
- **Determinism**: `bplapply` and `Parallel(...)` preserve input order. If you use `imap_unordered`, sort the collected results by a stable key before writing output — the package must be byte-reproducible across runs.

### Common embarrassingly-parallel cases in scRNA-seq

- Per-sample / per-library: SCTransform, Scrublet, QC filtering, Cell Ranger reuse-and-fix
- Per-compartment: integration (NP, AF, CEP independently), per-compartment clustering
- Per-cluster / per-cell-type: DE marker discovery, pseudobulk DESeq2 fit, GSEA per-comparison
- Per-permutation: GSEA / cell-type-proportion permutation tests, sensitivity sweeps
- Per-fold: cross-validation, integration-method comparison

### When NOT to fan out

- Sequential dependencies inside a single task (the output of unit N feeds unit N+1)
- One-shot work that fits in a single core-second
- Stage spec explicitly says "single-process" or `parallel_processable: false`
- Memory budget can't accommodate even 2 workers (use the on-disk libraries instead)

### Required logging

Append exactly one line per fan-out region to `runtime/outputs/<task_id>/progress.log`:

```
parallelism: detected_cores=<N>, recommended=<R>, usable=<U>, units=<K>, outer_workers=<W>, inner_threads=<T>, available_mem_gib=<M>, gpu=<g-or-none>
```

This lets the SME confirm the budget was actually used. If `outer_workers=1` despite `units > 1` and `usable > 1`, also log `parallelism_skip_reason=<one of: serial-deps | memory-budget | unit-too-small | spec-forbids>`.

## Per-task progress reporting (drives the live UI progress bar)

The server reads `runtime/outputs/<task_id>/progress.log` to render a live progress bar in the SME's web UI. To make that bar determinate (concrete N/M instead of an indeterminate shimmer), append exactly one structured marker line per phase as you advance:

```
[step <N>/<M>] <one-line description of the current phase>
```

The server picks the MOST RECENT `[step N/M]` line in the log; emit a new one whenever you transition phases. If your estimate of M changes mid-task (e.g., you discovered a sub-step you hadn't planned for), it's fine to revise — the bar updates honestly.

### When to emit

- Plan your task into a small number of distinct phases (typically 3-7) BEFORE you start, and emit `[step 1/<M>] <plan-line>` as one of your first progress.log lines.
- Append a new `[step N/M]` line when you START each phase (not when you finish). The SME's bar reflects "currently in phase N of M."
- For embarrassingly-parallel fan-out, the steps are the phases of the whole task (download → parse → validate), not per-unit. Per-unit progress goes through the parallelism-log channel.
- For a one-phase task that genuinely has nothing to subdivide, emit `[step 1/1]` once and skip the marker thereafter; the bar will sit at 100% but that's accurate.

### Example for a 5-phase data acquisition task

```
[step 1/5] resolving 7 GEO accessions to download URLs
[step 2/5] downloading 47 supplementary files in parallel (outer_workers=8)
[step 3/5] parsing matrix files into per-sample 10x mtx triplets
[step 4/5] validating cohort manifest against expected_libraries=47
[step 5/5] writing per_accession_summary.json + matrices_index.json
```

This pattern lets every multi-phase task display real progress. Without it the bar falls back to expected_artifacts counting if the stage declares them, otherwise indeterminate.

## Package containment (everything stays in $PACKAGE)

The package is a self-contained, byte-reproducible artifact. EVERY script you author, every byte you download, every intermediate file you write, and every environment lock must land somewhere under `$PACKAGE/`. Nothing escapes — no `/tmp/`, no `$HOME/Downloads/`, no system-wide pip/conda/R caches the next runner won't have.

### Required layout under runtime/outputs/<task_id>/

- `scripts/` — every script you authored for this task. One file per logical step, named verb-first (e.g. `01_fetch_matrices.py`, `02_validate_columns.py`, `01_run_sctransform.R`, `pipeline.sh`). Include a shebang line and a comment block at the top stating: tool versions used, exact command line that invoked it, input artifact paths (relative to `$PACKAGE`), and output artifact paths. Do NOT use `Bash` heredocs that leave no on-disk script — every line of code that ran for this task must be replayable.
- `data/` — raw downloaded inputs (GEO matrix files, supplementary tables, reference indexes) IF this task fetched them. Otherwise reference upstream task outputs by relative path: `../<upstream_task_id>/<artifact>` — never absolute paths, never paths outside `$PACKAGE`.
- `intermediates/` — anything the script writes that isn't the final result but the SME might want to inspect (filtered count matrices, integration anchors, embedding matrices, fold-CV temp objects).
- `<final-artifact-from-spec>` — the headline output named in `task.spec.expected_artifacts`. Lives directly under `runtime/outputs/<task_id>/` so RO-Crate registers it.
- `figures/<figure_id>.png` — required figures per `spec.required_figures`.
- `env.lock` — resolved tool/library versions for reproducibility:
- R: `sessionInfo() |> capture.output() |> writeLines("env.lock")` OR `renv::snapshot()` if the package uses renv
- Python: `pip freeze > env.lock` OR `conda env export > env.yml`
- System tools: append `<tool> --version` lines for non-R/Python binaries you invoked (samtools, bcftools, fastqc, parallel, etc.)
- `progress.log` — per-iteration narrative (start, mid, end-with-decision)
- `parallelism.log` (or appended to `progress.log`) — the structured `parallelism: ...` line per fan-out region.

### Redirect tool caches into the package

Set these BEFORE invoking heavy tools:

```bash
export TMPDIR="$PACKAGE/runtime/outputs/<task_id>/tmp"
export XDG_CACHE_HOME="$PACKAGE/runtime/cache"
export R_LIBS_USER="$PACKAGE/runtime/r-libs"
export PIP_CACHE_DIR="$PACKAGE/runtime/cache/pip"
export HF_HOME="$PACKAGE/runtime/cache/huggingface"
mkdir -p "$TMPDIR" "$XDG_CACHE_HOME" "$R_LIBS_USER" "$PIP_CACHE_DIR" "$HF_HOME"
```

`runtime/cache/` is shared across all tasks in the package; per-task `tmp/` is task-scoped.

### When containment is impossible

Some tools refuse to write into a relative path (rare — usually length, permissions, or a hard-coded system dir). Document the deviation in `runtime/outputs/<task_id>/result.json` under a `containment_deviations` array with: tool name, absolute path used, reason, and a copy of any critical files mirrored back into `runtime/outputs/<task_id>/external_refs/`. The SME reviews deviations as part of the decision audit.

### Verification before completing

Before flipping the task state to `completed`, the agent runs:

```bash
find runtime/outputs/<task_id>/ -type f | wc -l   # must be > 0
test -f runtime/outputs/<task_id>/env.lock        # required
test -d runtime/outputs/<task_id>/scripts/        # required
ls runtime/outputs/<task_id>/scripts/*.{py,R,sh,smk} 2>/dev/null | wc -l  # must be > 0 unless task is pure-discovery
```

If any check fails, the task is incomplete — re-run the missing step or block with `containment_violation` as the blocker_kind.

## Local git versioning (snapshot every task)

The package is a git repo. Every task you complete creates a commit so the SME can diff runs, revert mistakes, and time-travel through the analysis.

### Bootstrap (first agent invocation only — idempotent)

```bash
if [ ! -d "$PACKAGE/.git" ]; then
cd "$PACKAGE"
git init -q -b main
git config user.name "ecaa-workflow-agent"
git config user.email "agent@ecaa-workflow.local"
cat > .gitignore <<'EOF'
runtime/cache/
runtime/r-libs/
runtime/outputs/*/tmp/
runtime/outputs/*/.heartbeat
*.pyc
__pycache__/
.Rhistory
.ipynb_checkpoints/
EOF
git add -A
git commit -q -m "package: initial emit"
fi
```

### Per-task commit (run at the very end of every task, BEFORE setting status to completed)

```bash
cd "$PACKAGE"
git add -A
if ! git diff --cached --quiet; then
git commit -q -m "task <task_id>: <one-line summary of what changed>

method: <method_id_used>
inputs: <upstream task_ids consumed>
outputs: <files written under runtime/outputs/<task_id>/>
parallelism: outer=<W> inner=<T> units=<K>
runtime_seconds: <elapsed>
agent_iteration: <i>
"
fi
```

The commit message header is what the SME sees in `git log --oneline`. The body lets `git log --grep` find specific runs. The `task <task_id>:` prefix lets `git log -- runtime/outputs/<task_id>/` show only one task's history.

### When a task re-runs (agent retry, rerun button, amend)

Don't delete the old commits. The new run's commit lands on top:

```bash
git commit -q -m "task <task_id>: rerun (iteration <N>)

reason: <why — amend / blocker / failed first attempt>
prior_commit: <sha of the previous task <task_id> commit>
... (other fields)"
```

Use `git log -- runtime/outputs/<task_id>/` for full task history; `git diff <prior_sha> HEAD -- runtime/outputs/<task_id>/` for what changed.

### When a blocker fires

Commit blocker artifacts before yielding to the SME:

```bash
git commit -q -m "task <task_id>: blocked (awaiting_sme_*)

blocker_kind: <kind>
options_offered: <count>
top_candidate: <name>
"
```

This way the SME's selection (when it lands) gets its own commit on top, and the diff cleanly shows what the SME changed. NEVER `git reset --hard` or `git rebase -i` — append commits, never rewrite history.
