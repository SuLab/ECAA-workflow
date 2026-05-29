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

### What to write (and only this)

Write everything under `runtime/outputs/$ECAA_TASK_ID/`. Do not touch
`WORKFLOW.json` or any other task's directory — the harness is the sole
writer of task state.

1. **`state.patch.json`** — the single authoritative state transition. A
   patch-merge envelope of the shape:

   ```json
   {
     "from": "running",
     "to": { "status": "completed" },
     "harness_run_id": "<ECAA_HARNESS_RUN_ID>",
     "dispatch_epoch": <ECAA_DISPATCH_EPOCH>
   }
   ```

   Copy `harness_run_id` and `dispatch_epoch` verbatim from the
   `ECAA_HARNESS_RUN_ID` and `ECAA_DISPATCH_EPOCH` environment values so
   the harness can reject a stale patch from a superseded dispatch. `to.status`
   is one of `completed` | `blocked` | `failed`.

2. **`result.json`** — the task result: `task_id`, `status`, a short
   narrative, the artifacts you produced, and (for analytical stages) the
   figures you rendered. On a blocked exit include `blocker_kind` and a
   `what_would_unblock` note.

3. **`progress.log`** — append a human-readable line at each meaningful
   step. The harness reads recent activity here as a liveness signal; a
   long-running step with no progress line can look like a stall.

4. **`runtime/LOG.jsonl`** — append one JSON object per line for audit
   context (decisions, tool invocations, observations). Never write to
   `runtime/decisions.jsonl` — that file is owned by the conversation/server
   layer and carries only the typed `DecisionRecord` taxonomy.

### Figures obligation

If the task spec lists `required_figures`, you must produce each one. The
package bundles the rendering library under `runtime/plotting/` (and
`runtime/plotting_r/`); render figures with it from this task's real
result tables — do not stub, fabricate, or copy placeholder images. Write
the figure files under `runtime/outputs/$ECAA_TASK_ID/` and list their
paths in `result.json::figures`. A completed analytical stage that is
missing its declared figures is not done.

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
installed at task start (`pip`/`conda`/`BiocManager`), but prefer in-image
tools when method scores are close — every install spends wall-clock and
turns. All artifacts you write under `runtime/outputs/$ECAA_TASK_ID/`
persist into the emitted package on the host.
