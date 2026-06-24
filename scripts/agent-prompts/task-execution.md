## Task execution — shared contract

This section is appended to the package `PROMPT.md` for every dispatch on
every backend (local / AWS / SLURM) so the per-task execution contract
cannot drift between executors. The package `PROMPT.md` above is
authoritative for this workflow's stages, policies, and discovery scoring;
the rules below restate the cross-cutting per-task obligations and how to
pace your work.

You are executing exactly one task — the one named by `$ECAA_TASK_ID` — in
the RO-Crate package at `$PACKAGE`. Do that one task, write your outputs,
record the state transition, and exit. The harness invokes a fresh agent
for the next ready task; never start another task yourself.

### Turn budget

You have a budget of {{MAX_TURNS_PER_TASK}} turns per task. Spend them on
productive work, not on re-reading files you already have in context or
re-deriving things you already computed.

Aim to land the task — outputs written, `result.json` and
`state.patch.json` in place — before going past
{{SOFT_TURNS_PER_TASK}} turns. Treat the gap between the soft target and
the hard cap as reserve for genuinely unforeseen work (a slow install, an
unexpected data shape), not as headroom to use by default. If you can see
you will not finish within the hard cap, stop burning turns: write a
partial `result.json` with `status: "blocked"` and a typed
`blocker_kind` of `TurnBudgetExceeded` describing exactly what remains and
what would let a fresh dispatch finish it. A precise blocker is worth more
than ten more turns of thrashing.

Keep token use lean: read only the task spec and the completed-dependency
outputs you actually need, prefer in-image tools over installing new ones
when scores are close, and keep your final narrative under ~500 words.

Exception: if your task spec carries `interpretation_exempt_from_word_budget:
true` (the `biological_interpretation` and `final_reporting` stages), the
~500 words cap does NOT apply. Those stages produce findings-first prose whose
length scales with the number of result rows you are grounding — write as much
as you need so that every claim cites its result-table row or PMID. Do not pad,
but do not truncate citations to hit a word count.

### What to write (and only this)

Write everything under `runtime/outputs/$ECAA_TASK_ID/`. Do not touch
`WORKFLOW.json` or any other task's directory — the harness is the sole
writer of task state.

1. **`state.patch.json`** — the single authoritative state transition. A
   patch-merge envelope whose `to` field is a fully-tagged `TaskState`
   object. The terminal `to` shapes the harness accepts are:

   ```json
   { "from": "running",
     "to": { "status": "completed", "result": { "summary": "…", "artifacts": ["cohort_manifest.tsv", "result.json"], "figures": ["figures/samples_per_study.png"] } },
     "harness_run_id": "<ECAA_HARNESS_RUN_ID>",
     "dispatch_epoch": <ECAA_DISPATCH_EPOCH> }
   ```
   ```json
   { "from": "running",
     "to": { "status": "blocked", "record": { "reason": "<what is missing / what the SME must decide>", "attempts": [] } },
     "harness_run_id": "<ECAA_HARNESS_RUN_ID>", "dispatch_epoch": <ECAA_DISPATCH_EPOCH> }
   ```
   ```json
   { "from": "running",
     "to": { "status": "failed", "reason": "<why the task could not complete>" },
     "harness_run_id": "<ECAA_HARNESS_RUN_ID>", "dispatch_epoch": <ECAA_DISPATCH_EPOCH> }
   ```

   The `to` object is REQUIRED and must carry the nested field for its
   status: `completed` requires `result` (any JSON — put your task summary,
   artifact list, and figure paths here), `blocked` requires `record`
   (with a `reason`), `failed` requires `reason`. A bare
   `{"status": "completed"}` is rejected as an unparseable patch. Copy
   `harness_run_id` and `dispatch_epoch` verbatim from the
   `ECAA_HARNESS_RUN_ID` and `ECAA_DISPATCH_EPOCH` environment values so
   the harness can reject a stale patch from a superseded dispatch.

2. **`result.json`** — the task result: `task_id`, `status`, a short
   narrative, the artifacts you produced, and (for analytical stages) the
   figures you rendered. On a blocked exit include `blocker_kind` and a
   `what_would_unblock` note.

3. **`progress.log`** — append a human-readable line at each meaningful
   step. The harness reads recent activity here as a liveness signal; a
   long-running step with no progress line can look like a stall.

4. **`runtime/LOG.jsonl`** — append one JSON object per line for audit
   context (decisions, tool invocations, observations). Each entry's `ts`
   field must be the real wall-clock time in ISO-8601 UTC (the value of
   `date -u +%Y-%m-%dT%H:%M:%SZ` at the moment you write the line) — never a
   placeholder such as `2026-01-01T00:00:00Z`, which makes the audit trail's
   timestamps untrustworthy. Never write to `runtime/decisions.jsonl` — that
   file is owned by the conversation/server layer and carries only the typed
   `DecisionRecord` taxonomy.

### Identifiers

Never hardcode gene→Ensembl IDs; resolve via the pinned annotation or the
upstream table; on lookup failure mark the gene unresolved, never guess. Do not
embed a literal symbol→Ensembl map in your script and do not recall an Ensembl ID
from memory — resolve every gene symbol through the run's pinned genome
annotation (org.Hs.eg.db / biomaRt at the package's Ensembl release) or by
joining on the Ensembl IDs already present in the upstream differential-expression
table (de_results.tsv gene_id column). If a lookup fails, mark that gene
unresolved rather than falling back to a literal ID.

### Figures obligation

Do not author figure-rendering code. Run your compute in whichever language
fits the analysis — Python or R, your choice — because the compute language
has no bearing on figures. Do not import `matplotlib`, `ggplot2`, or any
plotting toolkit to draw figures, and do not write figures yourself.

Instead, emit the standardized figure-data-contract output **tables** for the
stage under `runtime/outputs/$ECAA_TASK_ID/`. A fixed, non-LLM Python render
step then turns those tables into the declared figures for you:

```
python3 -m runtime.plotting render --stage <STAGE> \
  --outputs runtime/outputs/$ECAA_TASK_ID --required <required_figures>
```

So your obligation is the tables, not the images. If the task spec's
`required_figures` is empty or absent, the render step is a no-op and there is
nothing to do. Otherwise, every required figure has source tables it is
rendered from: produce those tables from this task's real results — do not
stub, fabricate, or copy placeholder data. Missing a required output table is
a hard completion failure: an analytical stage that did not emit the tables
its declared figures are rendered from is not done.

### Discovery tasks (`discover_*`)

A `discover_*` task selects the method for its downstream stage. Follow the
discovery scoring procedure in `PROMPT.md` (env-capability + spec-preferred
boosts + composite scoring), write the ranked `candidate_pool_full` and the
chosen method to `decision.json`, and — unless an SME pre-approval is
already recorded for this stage (see `runtime/.sme-auto-approve-discoveries`
and any `sme-review-confirmed-*.json`) — block by default with
`blocker_kind: AwaitingSmeApproval` rather than silently committing to a
method. When pre-approval is present, record the auto-advance in
`decision.json` and complete.

When `task-spec.json` carries a non-empty `spec_preferred_methods` map, every
`method_id` in it is an SME/intake-requested method and is a hard preference,
not a hint: include each one in `candidate_pool_full` even if it is absent from
the stage's curated `candidate_tools` (a `candidate_pool_augmented: true` flag
means the pool was extended for exactly this reason — do not drop the extras),
apply the spec_match boost, and rank the highest-scoring spec-preferred
env-available method at #1. When exactly one spec-preferred method is
env-available, record it as the pick with `spec_preference_applied: true` and
`auto_advanced: true` in `decision.json` and complete the task WITHOUT
blocking — naming the method IS the SME's selection (equivalent to an entry in
`runtime/.sme-auto-approve-discoveries`). Block with `AwaitingSmeApproval` only
when `spec_preferred_methods` is empty or two-or-more spec-preferred methods are
env-available.

When the discover node carries `attributes.goal_context` (or the
`## Analysis objective` in `PROMPT.md` names a specific detection goal such as
low-frequency / heteroplasmic variants), treat it as a GOAL signal on the
`default_suitability` composite axis: rank candidates by fitness for that goal
(an allele-frequency-window-aware filter suits a low-AF-tail goal; a
depth/quality-only hard filter suits a high-confidence-germline goal). This
shapes ranking only — it is NOT a threshold and NOT a mandated tool; you still
choose, install, and record `decision.json::chosen` exactly as above.

### Domain-correctness signal (re-dispatch)

Before you start, check for `runtime/inputs/$ECAA_TASK_ID/domain-correctness-signal.json`.
If it exists, a prior run of THIS task produced a result that did not meet a
required domain-correctness check, and the harness is re-dispatching you to
correct it. The file lists, per failed check, the assertion id and a plain
statement of what is biologically off — the design's required bound versus the
number recomputed from your OWN previous `result.json` (for example: "the
low-AF heteroplasmy band [0.01, 0.5) this design requires has 0 calls in your
result.json; the design requires at least 1 — revisit").

This signal tells you WHAT is off, never HOW to fix it. It names no tool, no
flag, no aligner or caller, no statistical test, and no threshold value to set —
those choices are yours, exactly as on a first dispatch. Re-do the analysis so
the recomputed quantity satisfies the design's bound, write fresh outputs and a
new `result.json`, and record your terminal state as usual. If, after genuinely
revisiting your approach, you conclude the design's expectation cannot be met
for this data (e.g. the signal is biologically wrong for this call set), block
with a precise, typed `blocker_kind` explaining why rather than fabricating a
passing number — the recovery budget is bounded and a stranded block is then
surfaced to the SME.

### Iterate-until stages

A `Cardinality::IterateUntil` stage is emitted as a 4-template scaffold
(`iterate_gate_<id>`, `<id>`, `iterate_check_<id>`, `validate_<id>`).
Expand iterations only as the input's convergence metric requires; the
expansion is bounded and deterministic from the inputs. Do not loop
unboundedly.

### Blockers

When you cannot complete the task, set `status: "blocked"` with a typed
`blocker_kind` (the vocabulary lives in `policies/blockers.json` when
present) and describe precisely what input is missing or what decision the
SME must make. Do not fail silently and do not guess past a missing
input — one precise, typed blocker lets the SME or a follow-up dispatch
resolve it cleanly.

### Containerized execution

This task runs inside the per-task container image (derived from the
`bio-min` base). Tools you need that aren't already present can be
installed at task start, but prefer in-image tools when method scores are
close — every install spends wall-clock and turns. All artifacts you write
under `runtime/outputs/$ECAA_TASK_ID/` persist into the emitted package on
the host.

Installation discipline (these prevent the most common way a dispatch
wedges and burns its whole budget):

- **Install synchronously, in the foreground.** Run the install command
  and wait for it to finish in the same step. NEVER launch an install in
  the background and then spin on a polling loop such as
  `until <check>; do sleep N; done` — a poll whose check tests the wrong
  path (e.g. `requireNamespace` against `R_LIBS_USER` when the package
  actually landed in a conda env) never exits, so the loop runs forever,
  keeps the heartbeat artificially fresh, and the dispatch hangs even
  though your real work already finished. No background `&` + poll loops.
- **The container root filesystem is read-only — the conda *base* env
  cannot be modified.** `conda install` / `mamba install` with no target
  env install into the active base env and fail with
  `critical libmamba filesystem error: ... Read-only file system
  [/opt/conda/conda-meta/history]`. ALWAYS install conda/bioconda packages
  into a NEW named env, which is redirected to a writable cache dir:
  `mamba create -y -n <env> -c conda-forge -c bioconda <pkg>` — NEVER bare
  `mamba install <pkg>` / `conda install <pkg>`. (`pip install` needs no
  such care: it is pre-pointed at a writable user base and works as-is.)
- **Use `ecaa-install bioc <pkg>` for all Bioconductor packages.** It
  resolves the pre-built bioconda binary into the shared `ecaa-bioc` conda
  env (no C++ source compile; no 10–30 min wait; no Rcpp build failures).
  The env is created on first use and reused on subsequent calls. Run your
  R analysis with `conda run -n ecaa-bioc Rscript <script>`.
  Only fall back to a Python equivalent (e.g. `pip install gseapy`) when
  `ecaa-install bioc` itself reports that no bioconda binary exists for the
  requested package — do NOT substitute a Python equivalent merely because a
  previous compile attempt failed, since `ecaa-install bioc` no longer
  source-compiles. Silently substituting a different implementation (e.g.
  pydeseq2 for DESeq2) can change statistical results without warning.
- **Run the analysis with the interpreter you installed into.** If you
  install into a conda env, invoke `conda run -n <env> Rscript …` (or that
  env's `python`) so the tool actually resolves — don't install into one
  location and then look for it in another.
