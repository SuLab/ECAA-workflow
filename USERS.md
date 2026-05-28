# USERS.md — guide for SMEs running an analysis

**Audience:** bioinformatics domain experts ("SMEs") running an analysis through the chat UI. Not for developers — the React / Rust / CLI reference is in [`README.md`](README.md) and [`CONTRIBUTING.md`](CONTRIBUTING.md).

This document has three parts:

- **Part I — Walkthrough (§§1–4)** is a linear read for first-time users: what the tool does, a complete IVD scRNA-seq session, the right-hand State Inspector pane, and what the Accept button actually does.
- **Part II — Reference (§§5–11)** is a lookup manual: the full blocker taxonomy, session lifecycle states, lifecycle operations (rerun / revise / branch / sensitivity-winner / iteration), the canonical lotz v1→v5 walkthrough, the runtime artifact index, the decision-log variants, and which Claude model fires when.
- **Part III — Reading the package + getting help (§§12–16)** covers cost / time / privacy, figure resolution paths, a glossary, common troubleshooting, and where to ask questions.

---

# Part I — Walkthrough

## 1. What this tool does for you

You describe your analysis in plain English. The system builds a **plan** (an ordered set of analysis steps called *stages* — QC, normalization, clustering, and so on — connected in a task graph) and shows it to you. You **accept** it (the button click that freezes the *scope* of the analysis and writes an **emitted package** — a self-contained directory with the plan, policies, and later the results). The system executes the plan task by task. You review results as they come in and ask for changes in chat — **rerun** a step (same method, run again), **revise** a step (swap the method at one stage — what the system calls an *amendment*), or **branch** off (clone the session to try an alternative without losing the current run). No one edits a script on your behalf; your chat messages and your button clicks drive the entire analysis. Every button click is written to a typed **decision log** so you can audit later what you decided and why.

The system does **not** recommend a statistical test, an aligner, or a clustering algorithm. It picks best-practice defaults based on what it finds in your data, and tells you what it picked. If you have a specific **method** in mind (the concrete algorithm or tool for a stage — `DESeq2`, `Harmony`, `scVI`, etc.), name it in chat and the system will use exactly that. If the defaults produce something you disagree with — the number of clusters, the direction of an effect, the fold-change threshold — you revise or rerun from the chat.

---

## 2. A complete walkthrough

The example below uses the IVD single-cell scenario. Substitute your modality, sample count, and control/treatment labels in the dialogue patterns.

### Starting the conversation

Open the web UI. You'll see a greeting turn from the assistant:

> *Hi! Tell me about the analysis you're planning — what kind of data, what question, what you've already done. I'll work through it with you and pull together the package when we're ready.*

Describe your project in your own words. A first message like this is fine:

> We have 47 single-cell libraries from IVD patients — 23 degenerated, 24 healthy matched controls — and we want to find which chondrocyte subpopulations are depleted in disease.

Behind the scenes, the system is classifying the prose into a **modality** (`single_cell_rnaseq` here — the high-level assay category that drives which stage taxonomy gets used) and extracting **intake facts** (a small structured dictionary: sample count, organism, comparison axis, deliverables). You don't see any of that directly; you just see the follow-up questions. The assistant will ask one at a time if anything material is missing (species, reference genome, comparison contrast, etc.). Answer in one line; you don't need to be formal.

### Reviewing the plan

After a few turns, the assistant will propose a plan summary. It looks like this:

> **Here's the plan:**
> • **Samples:** 47 single-cell libraries (23 disease / 24 control)
> • **Reference:** human GRCh38, Cell Ranger-compatible references
> • **Pipeline:** QC → normalization → integration → clustering → cell-type annotation → differential abundance
> • **Deliverables:** depletion / enrichment table (cell type × condition), UMAP, supporting figures
>
> This reports statistical patterns, not causal claims. Shall I build it?
>
> **[ Accept ] [ Revise ]**

Read it carefully. This is the one moment where you commit to the shape of the analysis. Two buttons:

- **Accept** — the system finalizes the plan as a runnable package and stops asking you questions about the setup.
- **Revise** — you go back to chat. Tell the assistant what to change ("actually it's 25 disease not 23", "we're only comparing the NP compartment", etc.) and it will resend an updated summary.

> **Why this is a real commitment.** Once you accept, the system writes a self-contained package with your stated scope. That package becomes the source of truth for what runs. You can still change methods, rerun individual steps, or branch off later — but the *scope* (how many samples, which comparison, what deliverables) is locked unless you start a new session.

**If you're running a clinical trial,** the confirmation card includes two extra dropdowns before the Accept / Revise buttons:

- **Analysis discipline** — *Exploratory* (hypothesis-generating; no multiple-testing gates) or *Confirmatory* (pre-registered hypotheses only; stricter multiple-testing controls). Pick the one that matches your study design.
- **Checkpoint mode** — *Gated* (the plan blocks at every pre-registered checkpoint and requires your sign-off), *Selective* (blocks only at checkpoints declared as primary in the SAP), or *Fast* (runs every checkpoint without blocking; useful for dry-run rehearsals before the real trial analysis).

These are only shown when the classifier routes the project to the `clinical-trial-analysis` taxonomy (see the project-class section of [`docs/config-reference.md`](docs/config-reference.md)). Research-grade projects don't see them.

### Watching execution

Once you accept, the plan view on the right updates and a **"Start execution"** button appears. Click it. A card in the chat timeline confirms execution started, showing the process id and start time; the same card updates with the exit code when the run finishes. The Progress tab opens automatically and you'll start seeing lines like:

- Started: Quality checks on 47 libraries.
- Finished: Quality checks on 47 libraries.
- Started: Normalization method selection.
- Finished: Normalization — chose vst based on default scoring.

Each line is one task in the plan. The system batches rapid updates so you don't get a torrent — expect one synthesis message every few seconds during heavy activity. Switch to the Performance tab to see running cost / instance hours / remaining tasks; switch to the Plan tab to see the task graph with states colored as execution progresses.

**Quick-reply chips.** As the conversation progresses, the assistant sometimes offers a row of suggested-reply buttons under a message (e.g. *"Use the default threshold"* / *"Tell me the tradeoffs first"*). Clicking one sends that text as your next turn. The chips are shortcuts — you can always type a free-form reply instead.

**Auto-title.** After the first few turns, the system generates a short title for your session (e.g. *"IVD scRNA-seq — disease vs. control"*) so the session list stays readable when you have several open. The title is deterministic and you can rename it; the underlying session id never changes.

### Reviewing a result

When a task produces an output, it shows up as a result card in the chat timeline. The card has the step name, the status (completed / failed / blocked), a short description, and the result body — a numeric summary, a link to the figure, or the reason for a failure. You'll also see a **Rerun** button on each card.

If a claim in the narrative (e.g., "ACAN was upregulated in disease") disagrees with the underlying table, the system catches it and flags the card with a red "mismatch" banner. You read the mismatch details, decide whether the narrative or the table is correct, and either Accept the result (if the narrative is fine) or Revise (if the analysis needs to be rerun).

### Revising mid-flight: three ways

**Rerun a step.** You want the same analysis step run again — e.g. the clustering looked odd, upstream data got fixed, or you want a second attempt with the same method. Click **Rerun** on the task's result card. Type a short reason if you want ("upstream inputs refreshed"). The system reruns just that task and the tasks downstream of it.

**Change a method.** You want a *different* method used at a specific step — e.g. swap `harmony` for `scvi` at the integration step. Say so in chat: "use scVI for integration instead of Harmony." The system acknowledges, and asks you to accept a small card confirming the swap before it kicks off the new run. Tasks downstream of that step rerun; anything upstream stays.

**Branch to explore an alternative.** You want to try an alternative *without* losing the current run. Click **Branch from here** on the session. A new session is created that inherits everything you've described so far; the original session is unchanged and still live. Common use: "run the same analysis but with a tighter p-value threshold" — one branch for 0.05, another for 0.01.

### Finishing

When every task has completed and every result card is either cleanly completed or accepted-after-review, the system tells you the analysis is done and points at the final outputs (tables, figures, narrative). Those files are persisted under the emitted package; you can navigate there at any time from the plan view.

### Amendments, cross-version diff, and sensitivity comparisons

Three surfaces worth knowing about when you start iterating post-emission.

**Cross-version diff (after an Amendment).** When you Revise a method and the system re-emits, it writes a `cross_version_diff.json` that compares every result table row-by-row against the previous version. The result-review card for any affected task gets a **"Compare to previous version"** link. Click it to see an inline diff — which rows changed direction, which effect sizes moved, which p-values crossed the threshold. This is how you tell whether a method swap actually changed the answer or just the numbers near the edge.

**Sensitivity comparison.** Some stages in some taxonomies (e.g. single-cell integration) declare a `sensitivity_comparison` — the agent runs multiple candidate methods in parallel and then blocks on `awaiting_sme_selection`. When that happens, a **Sensitivity comparison card** appears in chat: each candidate has a scored row (best-practice scoring, read from `runtime/outputs/<task_id>/decision.json`), side-by-side thumbnails of the key output figures, and a radio button. Pick the winner and click **Accept selection and continue**. The runner-ups are retained under `runtime/sensitivity/<stage>/<method>/` so you can always revisit them.

**Branch from here.** Any time you want to try an alternative without giving up the current run, click **Branch from here** on any turn. You land in a new session whose transcript up to the branch point is a copy; everything after is independent. See §7 for the lifecycle-operations reference.

---

## 3. The State Inspector (the right-hand pane)

The right-hand pane has 11 tabs. You don't need to look at most of them most of the time; the chat drives everything. But they're worth knowing about for when you do.

### Plan

The task graph for your analysis, one node per step. Nodes color-code by state: blue = ready, grey = waiting, green = complete, red = failed, orange = blocked. Click a node to see its upstream and downstream dependencies and the method the agent picked. This is the tab to open when you want to see "what's happening right now" at a glance.

### Status

A raw JSON view of the current session state. Useful for debugging ("why is the session in `PendingConfirmation`?") but not day-to-day reading. Support will ask you to open this if something looks stuck.

### Documents

Placeholder today — reserved for document-oriented outputs (e.g. a rendered PDF of the final narrative).

### Inputs

Files you've uploaded to the session — phenotype tables, sample sheets, manual count matrices. The tab lets you inspect what the harness will see, delete the wrong file, or upload a missing one before kicking off execution. The harness reads from `runtime/inputs/` inside the emitted package, which is what this tab manages.

### Progress

The live feed of harness events. Each task that starts, finishes, or stalls shows up here as a line with a timestamp. When execution runs on a remote backend (AWS or SLURM), each line is tagged with the backend and the instance type (e.g. `[aws · m6i.4xlarge]`). The tab auto-switches the first time a progress event arrives, so you don't have to remember to watch it.

### Performance

Running cost and utilization for the current session. Three cost lines:

- **Chat** — the conversation with the assistant. Typically cents to a few dollars per session.
- **Agent** — the actual analysis runs (per-task executor cost). Typically dominates the bill.
- **Scorer** — only present if you clicked "Score transcript" (see below).

Plus: turn count, tool-call count, p50/p95/p99 latency, Sonnet / Opus turn split, token totals, instance-hours by instance type. The three cost lines sum to the headline "Total cost". A yellow **"high_water_exceeded"** row appears if the harness ever had to resize a task upward — worth checking if the bill is higher than expected.

**"Score transcript" button.** An operator-triggered rubric scorer. Clicking it fires a one-shot call that reads the entire session transcript and scores it on nine dimensions (naturalness, one-question-per-turn, method-neutrality, claim-boundary fidelity, etc.). Bills to the `scorer_cost_usd` line above. Use it for QA / calibration; not a routine action during an analysis.

### Figures

Gallery of every rendered figure (PNG / SVG) produced so far by completed tasks. Click a figure to expand it; figures are also surfaced inline on result cards in the chat.

### Dashboard

Interactive plots from completed tasks (UMAPs, clustering scatter plots, PCA, etc.). Empty before execution produces its first figure. This is where you go to explore the shape of the data — brush on a UMAP to select a cluster, hover a point to see its sample id and top-expressed genes.

### Decisions

Audit trail of every typed decision the system has recorded for this session — Confirm clicks, method amendments, sensitivity-winner picks, dispositions applied, budget changes, and so on. Each row is one entry in `runtime/decisions.jsonl`. Click a row to see the rationale and timestamp. Useful when you need to remember "did I approve that, and when?" or when a collaborator asks how a particular result came to be.

### History

If your session has been **branched** (see §7.3 below), this tab shows the parent/child graph of the whole lineage. Each branch is a separate session with its own audit trail; click a node to navigate to that session and compare results side by side. If you never branched, the tab shows a single node — just your current session.

### Compare

Side-by-side diff against a parent or sibling session — any time you've Revised (amended) or Branched, this tab renders the row-level concordance report (`runtime/cross_version_diff.json`) for every result table. Direction flips, effect-size deltas, and threshold crossings are highlighted. Empty for sessions with no parent.

---

## 4. What "Accept" actually does

The Accept gate exists for one reason: the system will never commit to a runnable analysis based on something you said in chat alone. Chat messages can be ambiguous, can be interrupted, can contain a speculative "what if". The button click is deliberate — it's the one moment where you tell the system "yes, this is exactly what I want you to run."

Three things happen the moment you click Accept:

1. **The plan is frozen.** The package is written to disk with every intake field, every method choice, and every dependency edge you discussed. That package is what executes.
2. **The confirmation is audited.** A decision record is appended to the session's audit log with a timestamp, a copy of the plan text, and your rationale (if you provided one).
3. **The assistant switches modes.** The conversation changes from setup-focused to execution-focused. You'll see status updates and result cards instead of intake questions.

What Accept does **not** do: it doesn't start execution automatically. You still have to click **Start execution** on the plan view. This two-step design lets you accept now and kick off the run later (or have someone else kick it off on a machine with more capacity).

Accept commits to a **scope**. It does not commit to a specific method, aligner, threshold, or figure.

**What Accept freezes:**
- The sample count and groupings.
- The comparison / contrast.
- The reference genome / organism.
- The modality-specific stage structure (DAG shape).
- The deliverable list (tables, figures, narrative sections).

**What stays revisable after Accept:**
- Every method choice at every `discover_*` step — you can swap via chat ("use scVI for integration").
- Every numeric threshold with a `discover_*` parent.
- Any individual task — rerun at will.
- The whole downstream slice — branch to explore an alternative.

**What is genuinely hard to change after Accept:**
- The sample count. Adding samples means a new session.
- The modality. Changing from single-cell to bulk means a new session.
- The comparison shape (disease vs. healthy → timecourse). New session.

If you're uncertain about any of the "hard to change" items, click Revise and refine the plan before accepting.

**The decision log.** Every Accept click appends a `DecisionRecord` to `runtime/decisions.jsonl` in the emitted package. Reject (Revise), Unblock, and Branch clicks do the same, and so does every LLM-driven mutation (method amendment, rerun, sensitivity winner). Read the log for reproducibility, audits, and "what did I decide three weeks ago?" questions. Each record is typed (`type`, `actor`, `timestamp`, `payload`, `rationale`); see [`crates/core/src/decision_log.rs`](crates/core/src/decision_log.rs).

---

# Part II — Reference

## 5. Blocker taxonomy

Partway through a run, a task can hit a condition the system can't resolve on its own. A **Blocker** card pops up in the chat, visually distinct from regular results: orange stripe, a plain-English title, and recovery affordances.

**Common blocker kinds at a glance:**

- **Data doesn't match the expected shape** — input has the wrong columns / rows / sample count. Fix upstream, then continue.
- **A validation check failed** — a `validate_*` subtask flagged a quality problem. Accept the check or swap the method.
- **A metric landed below threshold** — a numerical gate fell below policy. Accept or swap.
- **An upstream task hasn't produced its output yet** — usually self-heals; wait for the parent.
- **The agent hit an error it couldn't recover from** — rerun the task, or ask for a method swap.
- **The host system reported an error** — infrastructure issue (OOM, disk, network). Retry / Unblock once cleared; contact your operator if it persists.
- **Your input is needed to pick between alternatives** — scoring tied. Pick a winner.
- **The pilot projection exceeds your cost ceiling** — pilot run projected a cost above your `SWFC_AWS_COST_CEILING_USD`. Rescope or accept the overage.
- **A running task looks stuck** — CPU starvation, memory pressure, or runtime-over-expected. Three-button recovery: Resize and resume / Retry / Abort.
- **A validation contract assertion failed** — a policy-declared contract (e.g., "every sample needs `cells_per_sample_min`") failed. Accept or revise.

Example — data-shape mismatch:

> **The data doesn't match the expected shape**
> The system tried to load the degenerated-vs-healthy comparison but found 23 control samples, not the 24 you specified. The extra sample "P47" isn't in the control list.
>
> Recovery: either correct the sample count above or add P47 to the control group before continuing.
>
> **[ I've addressed this — continue ]**

Different blockers get different affordances — a validation failure asks you to accept the check or choose an alternative method; a stalled task gives you resize/retry/abort buttons; a method-selection blocker shows you the candidate methods and their scores so you can accept the top pick or override to a runner-up.

Rule of thumb: read the title, read the recovery hint, fix the condition (or choose the alternative), then click the recovery button. If you're stuck, type a question into chat and the assistant will explain what it knows.

### The full taxonomy

A **Blocker** is a runtime condition the system can't resolve without your input. Each blocker has a *kind* that determines which recovery affordance the UI shows you. Every blocker card shows you:

1. A plain-English title (per the table below).
2. The agent's reason, stripped of internal paths and stage IDs.
3. A recovery hint.
4. A button (or button group) whose label depends on the kind.

| kind | title | triggers | recovery |
|---|---|---|---|
| `data_shape_mismatch` | "The data doesn't match the expected shape" | A task's required input has the wrong columns / rows / sample count / metadata. | Manual fix upstream (correct the intake field, add a missing file, update a sample sheet), then **I've addressed this — continue**. |
| `validation_failed` | "A validation check failed" | A `validate_*` subtask found a quality problem that a policy contract flagged as blocking. | **Accept and continue** (override the check) or switch the upstream method by typing the swap in chat. |
| `metric_below_threshold` | "A metric landed below the acceptable threshold" | A numerical gate (silhouette score, contamination, mapping rate) fell below the policy threshold. | Same as `validation_failed` — accept the result as-is or swap the method. |
| `missing_input` | "An upstream task hasn't produced its output yet" | The DAG scheduled this task but an input it needs isn't on disk. | Usually self-heals — the parent task hasn't finished. Click continue only after the upstream finishes. |
| `agent_error` | "The agent hit an error it couldn't recover from" | The executing agent (whatever ran the step) crashed or returned a structured error. | **Rerun** the task, or read the error and ask for a method swap in chat. |
| `host_error` | "The host system reported an error" | The machine running the task had an infrastructure problem (OOM, disk-full, network timeout). | **Retry** / **Unblock** once the host-side issue has cleared. For persistent host errors, contact your operator — resize-and-resume is a stall-specific affordance, not a host-error one. |
| `awaiting_sme_selection` | "Your input is needed to pick between alternatives" | Best-practice scoring tied, or a `sensitivity_comparison` stage needs a winner. | Radio-button picker with candidate methods + scores; click the one you want and **Accept selection and continue**. |
| `pilot_oversize` | "The pilot projection exceeds your cost ceiling" | The pilot sizing run projected a full-run cost above `SWFC_AWS_COST_CEILING_USD`. | Accept the projection (continue at higher cost) or rescope the analysis. |
| `stalled` | "A running task looks stuck" | CPU starvation / memory pressure / idle GPU / runtime-over-expected. | Three-button group: **Resize and resume** / **Retry** / **Abort task**. |
| `contract_violation` | "A validation contract assertion failed" | A policy-declared contract (e.g., "every sample must have a cell count > 500") failed after the task completed. | Accept or revise — similar to `validation_failed`. |
| `runtime_capability_missing` | "A required system capability isn't available" | The agent reported a missing CLI tool, library, or runtime extension (e.g., `bcftools` not on PATH, `scvi-tools` import failed). | Provision the missing tool on the host (or for a remote backend, contact your operator), then **Retry**. |
| `awaiting_structured_decision` | "A structured choice is required from you" | The agent emitted a `decision_point` payload requesting a typed answer (radio / checkbox / dropdown) before continuing. | The Blocker card renders the structured form; pick an option and **Submit**. |
| `awaiting_sme_approval` | "Approval required before continuing" | A deviation, an out-of-band cost projection, or a confirmatory-mode protected action needs explicit go-ahead. | **Approve** or **Reject**, optionally with a rationale that lands in the audit log. |
| `missing_artifact` | "A required artifact wasn't produced" | A task completed but didn't emit the artifact a downstream stage requires (e.g., a `.h5ad` file). | **Rerun** the task; if it persists, swap the upstream method via chat. |
| `heartbeat_stalled` | "The running task stopped sending heartbeats" | The agent's `.heartbeat` file hasn't been touched within `SWFC_TASK_HEARTBEAT_STALL_SECS` (default 15 min). | **Retry** — the harness will re-dispatch. If the agent is provably alive (long compute), bump the threshold via env var. |
| `orphaned_by_crash` | "A prior task lost track of its dispatch" | The harness restarted while a task was running and the WAL recovery couldn't prove the prior dispatch was alive within the liveness window. | **Retry** — usually self-healing. |
| `tool_error` | "The agent reported a structured error" | The agent wrote `runtime/outputs/<task_id>/error.json` with a typed envelope (OOM, MissingDependency, NumericalInstability, etc.). The Opus 4.7 `remediation_proposer` side-call drives a ranked list of suggested fixes the SME can one-click apply. | **Apply suggestion** (top-ranked remediation) / **Try alternative** (browse all 3) / **Manual review** (open the task drawer). Capped at 5 attempts before escalation. |
| `image_digest_mismatch` | "The container image digest doesn't match what was pinned" | At task start the runtime resolved a digest different from the one pinned in `WORKFLOW.json` / `policies/container.json`. Indicates registry tampering, a re-tagged floating tag, or a stale local cache. | **Rerun** after pruning the local cache; or re-emit the package to accept the new digest. |
| `container_pull_failed` | "Couldn't pull the container image from the registry" | The runtime (Apptainer / Docker / podman) failed to pull (HTTP 401 unauthorised, HTTP 404 not found, transient network failure). | **Retry**, swap the image, or configure registry credentials. |
| `container_start_failed` | "The container image pulled, but the runtime couldn't start it" | Pull succeeded but exec failed — missing GPU driver on the host, kernel ABI mismatch, unsupported OS, missing capabilities. | **Retry** on a different host; fall back to host-env via `SWFC_DISABLE_CONTAINERS=1`; or pivot to a CPU image when the failure is GPU-related. |
| `runtime_missing` | "No container runtime is available on this host" | Apptainer / Docker / podman aren't on the host's PATH. The harness probes runtimes in priority order; this kind fires when none are available. | Install a runtime; set `SWFC_CONTAINER_RUNTIME=<name>` to point at a non-default install; or set `SWFC_DISABLE_CONTAINERS=1` to fall through to the host environment. |
| `sbom_emission_failed` | "The supply-chain SBOM couldn't be written" | Syft SBOM emit at task completion failed. The task itself completed successfully but the supply-chain attestation couldn't be written. | **Rerun** the SBOM emit standalone, or set `SWFC_SBOM_EMIT=0` to skip on subsequent tasks. |
| `network_policy_violation` | "A task tried to use the network while the policy forbids it" | The task attempted egress while running with `network: none` (typically the `clinical_trial` archetype per ADR 0028). | Amend the task's container `network` policy to `bridge`, or remove the network-dependent step from the analysis. |
| `container_cache_corrupted` | "The per-session container cache appears corrupted" | The cache mount detected on-disk corruption (truncated layer, checksum mismatch, OverlayFS inconsistency). | Prune the cache via the **prune-container-cache** action, then rerun. |
| `memory_exhausted` | "The scheduler killed this task for exceeding its memory cap" | SLURM reported `OUT_OF_MEMORY` or AWS reported SIGKILL with peak RSS at the cgroup limit. Distinct from the rate-of-change `Stalled { signal: MemoryPressure }` — this one fires *after* the kill. | **Rerun with more memory** — the executor offers a one-click resize action that lifts the memory cap. |
| `time_exceeded` | "The scheduler killed this task for exceeding its wallclock cap" | SLURM reported `TIMEOUT` (the task hit the `--time=` cap) or AWS reported wallclock past `expected_secs * SWFC_RUNTIME_MULTIPLIER`. Distinct from `Stalled { signal: RuntimeOverExpected }` which is the heads-up; this fires *after* the kill. | **Rerun with longer time limit** — the executor offers a one-click extend-and-rerun action. |
| `replay_corruption` | "A persisted record couldn't be loaded — session history is incomplete" | Decision-log replay or session-envelope upcasting hit a corrupted record while running in permissive mode. The bad record was skipped + appended to `runtime/sessions.errors.jsonl` for operator review. | **Acknowledge — continue with partial history**, or branch the session if the missing context is load-bearing. |
| `image_digest_unresolved` | "Couldn't pin a registry digest for `<image>:<tag>`" | The composer tried to pin every container's digest before WORKFLOW.json write but couldn't reach the registry, the requested tag returned 401/404, or the response wasn't a valid digest. | **Retry digest resolution** when registry is reachable; pin a different image:tag if it's removed; set `SWFC_DISABLE_CONTAINERS=1` to fall through to host-mode. |
| `composition_infeasible` | "The composer can't produce a valid plan from the current atom registry" | The composer can't reach the goal from any atom in the registry. Carries EDAM-typed `missing_inputs`, an optional `unreachable_goal`, and any `excluded_paths` the composer ruled out via CEL. | **Open composer recovery** — UI surfaces "Try adding…" affordances tied to ontology terms. Add the missing capability or amend the goal. |
| `container_exited_abnormally` | "The container exited with status `<exit_code>` (or was OOM-killed)" | The agent process inside the container returned a non-zero exit. When `oom_killed: true`, the kernel killed it inside the agent's lifetime. Distinct from `memory_exhausted` (scheduler-side classification) — this happens inside the cgroup. | **Rerun with more memory** when oom_killed is true; otherwise inspect the partial output set + amend the stage method. |
| `slurm_runtime_unavailable` | "Partition `<name>` doesn't support `<required-runtime>`" | The atom declares a container runtime requirement (e.g. `apptainer-1.4`) that the partition's `container_runtimes:` list doesn't include. `slurm/sbatch.rs::validate_submission` refuses to submit. | **Pick a different partition** when one exists with the runtime; set `SWFC_SLURM_NATIVE_CONTAINER=1` to use SLURM 25.11's `--container` directive when applicable; or amend the atom to drop the container requirement. |
| `container_hung` | "`<runtime>` container for `<task>` is alive but hung — host is fine" | The container-aware orphan reaper saw a stale heartbeat while the SSM/SSH probe found the container itself still alive. The host instance is healthy; only the in-container agent is wedged. | **Reap container & rerun** — preserves the host instance for retry instead of tearing it down. |
| `iteration_did_not_converge` | "`<task>` ran `<N>` iterations — metric `<x>` hasn't reached `<threshold>`" | An iterate-until atom hit `max_iterations` without satisfying the convergence rule for the required number of consecutive passes. Distinct from `Stalled` (the iteration is making progress, just not converging) and from `MetricBelowThreshold` (single failed validation gate). | Three-button picker — **Raise threshold** / **Accept best iteration** / **Abort task**. "Accept best" picks via `iterate.best_selector` if set, otherwise the last iteration. |
| `schema_version_mismatch` | "`<config_kind>` schema mismatch — expected `<expected>`, found `<found>`" | A config manifest (modality / archetype YAML, dispatch WAL, etc.) carries a `schema_version` the loader doesn't know how to bridge through the `core::migration::MigrationRegistry` chain. Typically surfaces when an emitted package authored against a newer schema is loaded on an older binary, or vice versa. Carries the `config_kind` so the recovery hint names exactly which file shape failed (e.g. `ModalityConfig`, `ArchetypeConfig`, `DispatchWal`). | **I've migrated — retry load** once the manifest has been migrated to the loader's expected version, or rebuild the binary against an upgraded schema. |

Every blocker kind carries a `task_id` so the UI can link to the specific card. The live enum definition is in [`crates/core/src/blocker.rs`](crates/core/src/blocker.rs) and surfaces to the UI via the generated `BlockerKind` type. As of 2026-05-16 the enum has 40 variants; the table above groups the production kinds — see the live enum for the StallSignal substructure that drives the Stalled variant's three-button affordance. The enum is `#[non_exhaustive]`, so external consumers must always include a wildcard arm.

### 5.1 Claim verification

When a task produces a narrative artifact *and* its stage's policy declares `verifiableEntities`, the system runs a two-step check:

1. **Extract** claims from the narrative. A claim = (entity + direction + effect size + p-value + cited table). Driven by regex matching against known phrasings.
2. **Verify** each claim against the corresponding row in the cited `results/tables/*.tsv` file.

Three possible verdicts per claim:

- **Verified** — narrative and table agree on direction, effect size (within tolerance), and p-value. You can trust this one.
- **Mismatch** — disagreement in at least one dimension. Carries a `detail` string (e.g., "direction: narrative says Up, table effect size is -1.2"). The session transitions to `Blocked { ValidationFailed }`.
- **Unverifiable** — the extractor couldn't anchor the claim to a table row (no source-table citation, ambiguous entity). Carries a `reason` string; does not block.

The report surfaces in the result card as a compact summary ("3/5 verified, 1 mismatch, 1 unverifiable") with a Show detail button. When detail is expanded, each verdict renders with an icon (✓ verified / ✗ mismatch / ? unverifiable), the claim as extracted, and the mismatch / unverifiable reason.

Every completed task surfaces a result card. What's on it: the step name (e.g. "Differential abundance"); the status badge (**completed** green / **failed** red / **blocked** orange); a description of what was run; a body that varies by status (JSON for a completed task, a plain-English reason for a failure, a structured record for a block); a **Rerun** button; optionally, a **Claim verification** summary if the task produced a narrative section.

**How to respond to a mismatch.** Don't click past it. Either the narrative has a typo or the analysis has a real bug. Open the cited table, check the row yourself, decide which side is wrong:
- Narrative wrong → Revise the narrative (it'll regenerate next time the reporting stage runs).
- Analysis wrong → Revise the method upstream (likely the differential stage), then rerun forward.

This is not optional — it's the safety net for the v1-fabrication pattern (the original IVD draft had 14 fabricated gene claims; this check prevents that). If you see a mismatch, don't click past it without understanding which side is wrong.

---

## 6. Session lifecycle states

A session moves through a small state machine as you drive it. You don't need to memorize the state names; the UI shows the current one in the pill at the top-right of the state inspector. But if you see a state referenced in a log or a support conversation, the table below names them.

| State | Plain-English meaning | How you got here | What you can do |
|---|---|---|---|
| `Greeting` | The system is saying hello; no project described yet. | First load of the chat UI. | Type your initial project description. |
| `Intake` | Capturing the shape of the analysis from your messages. | Any first non-trivial message. | Keep describing; answer follow-ups. |
| `IntakeFollowup` | The system has enough to propose a plan; one or two details remain. | Mid-intake, most fields filled. | Answer the remaining questions. |
| `PendingConfirmation` | A plan summary is on screen waiting for Accept / Revise. | The assistant called `propose_summary_confirmation`. | Click Accept or Revise. |
| `ReadyToEmit` | You accepted; the package is being written. | Accept button clicked. | Wait (it's fast). |
| `Emitted` | The package is on disk, ready for execution. | Emission completed. | Click Start execution. |
| `Emitting` | Execution is mid-flight; the plan is running. | Start execution clicked. | Review result cards as they arrive; revise / rerun / branch as needed. |
| `Amending` | The system is reworking a step you asked to change. | You requested a method swap or rerun. | Watch the progress panel for the rework. |
| `Blocked` | A task needs your attention. | A blocker fired. | Use the affordance on the Blocker card (see §5). |

The live enum + transition table is in [`crates/conversation/src/session/mod.rs`](crates/conversation/src/session/mod.rs) and [`crates/conversation/src/session/transitions.rs`](crates/conversation/src/session/transitions.rs).

---

## 7. Lifecycle operations

These three operations all let you change something after the original plan was accepted. Pick the one whose shape matches your intent:

- **Rerun a task.** Same method, same everything — just run it again. Useful after an upstream data fix, a transient infrastructure issue, or a second attempt with the same defaults. Cheapest of the three.
- **Revise (amend) a method.** Swap a method at a specific step — different clustering algorithm, different normalization, different effect-size threshold. Upstream tasks keep their results; downstream tasks rerun with the new choice. This is the most common iteration.
- **Branch the session.** Try an alternative without giving up the current run. Branching creates a parallel copy of the session from this point forward. You can revise or rerun in either branch independently. Best for sensitivity analyses ("does the conclusion change under a stricter threshold?") and for what-if exploration ("what would it look like if we excluded samples P03 and P12?").

Three additional surfaces are worth knowing about when you start iterating post-emission. **Cross-version diff** (after an Amendment) writes a `cross_version_diff.json` row-by-row comparing every result table against the previous version; the result-review card for any affected task gets a "Compare to previous version" link. **Sensitivity comparison** is some stages' built-in sweep over multiple methods (e.g. single-cell integration runs Harmony / scVI / scanorama in parallel) — when the comparison resolves, a card appears in chat with side-by-side scored candidates and a radio button. **Branch from here** is available on any turn; the new session shares the transcript up to the branch point and is independent after that.

Four SME-initiated operations exist after the initial Accept. All four write a `DecisionRecord` and most rewrite part of the emitted package on disk.

### 7.1 Rerun

**Inputs:** a task id, optionally a rationale.
**What happens:** the system transitions the task back to `Ready` with its method choice unchanged, invalidates every downstream task, and reruns each of them in order once their upstream dependencies complete.
**Artifacts:** the system preserves the target task's existing output files under `runtime/outputs/<task_id>/_previous/` so you don't lose them, then writes the fresh run to the canonical path.
**When to use:** same analysis, want another attempt. Upstream data got fixed. A transient infrastructure error. You changed a hardware envelope and want to try again at larger scale.

### 7.2 Revise (method amendment)

**Inputs:** a stage id and a new method choice. You trigger it by naming the method in chat ("use scVI for integration"); the assistant then asks you to accept a small amendment card before rewriting anything.
**What happens:** the system swaps the stage's method, walks the DAG with `invalidate_forward_slice` to mark every downstream task dirty, and leaves upstream tasks untouched. The session flips to `Amending { target_stage, invalidated_tasks }` during the rewrite, then back to `Emitted`.
**Artifacts:** the system writes a fresh emitted package with `prov:wasDerivedFrom` pointing at the original and preserves the original package on disk.
**When to use:** the analysis itself is right, but you want a different method at one step.

### 7.3 Branch

**Inputs:** optionally a rationale.
**What happens:** the system mints a new session id with `parent_session_id` set and leaves the parent session unchanged. Harness events, tool calls, and the decision log all fork — the new session keeps its own audit trail.
**Artifacts:** the system hard-links (rather than copies) the emitted package into the new session's root, so disk usage stays reasonable even if you branch many times.
**When to use:** exploration without commitment. Sensitivity analyses. Parallel "what if we exclude sample P03" comparisons.

**Known limitation — SessionTree root walking:** the History tab renders the lineage graph from the *current* session walking outward via `parent_session_id` and `child_session_ids`, so the visible graph stays scoped to the connected component of whichever session you're viewing. If you've branched several independent root sessions (i.e., separate analyses with no shared parent), each renders its own SessionTree. There's no top-level "all sessions" walk that aggregates across roots; that would need a registry-level index `~/.scripps-workflow/sessions/index.json` enumerating root session ids, which we haven't built. To navigate between independent root analyses, use the session-list UI (top-left of the desktop layout) — it scans the sessions directory directly. This affordance is intentionally minimal: the SessionTree is meant to surface the lineage of *one analysis*, not be an analysis-management surface.

### 7.4 Sensitivity winner selection

**Inputs:** a stage id and the chosen winner.
**What happens:** for stages of the `sensitivity_comparison` class, the system runs multiple methods in parallel, then blocks on `AwaitingSmeSelection`. Your pick becomes the official result; the system retains the runner-ups under `runtime/sensitivity/<stage>/<method>/` so you can compare later.
**Artifacts:** same as Revise — an amendment package with `prov:wasDerivedFrom`.
**When to use:** when the plan explicitly runs a sweep (defined by the modality taxonomy — e.g., single-cell integration typically runs `sensitivity_comparison` across Harmony / scVI / scanorama).

### 7.5 Iteration (Cardinality::IterateUntil)

**What it is.** Some stages run a method **iteratively** until a quality metric crosses a threshold for several consecutive passes. Examples: clustering algorithms that re-run with adjusted hyperparameters until silhouette score stabilizes; gradient-descent-style fits that stop when the loss flattens; cell-cell communication estimation that re-samples until the false-discovery rate drops below the gate.

**What it isn't.** Iteration is **not** the same as a sensitivity sweep (§7.4). A sensitivity comparison runs N different methods in parallel; iteration runs the SAME method N times tweaking its inputs each pass. The same atom can support either, but the two surfaces are independent.

**The DAG shape.** When a stage is iterative, the compiler emits a 4-task scaffold up front (`iterate_gate_<stage>`, `<stage>` placeholder, `iterate_check_<stage>`, `validate_<stage>`). The agent then fans out a linear chain of `<stage>_iter_1`, `<stage>_iter_2`, ... at runtime, one per pass, until the convergence rule fires. You see the chain populate in the State inspector tab as the agent runs.

**Convergence rule.** Set in the atom's `iterate.convergence` block: a metric-source CEL expression (e.g. `result.silhouette`), an operator (`gt` / `lt` / `gt_eq` / `lt_eq`), a threshold, and a number of consecutive passes that must satisfy the rule (default 2 — one stable pass rules out a stochastic dip).

**`max_iterations` ceiling.** Hard cap declared on the atom. When reached without convergence, the session transitions to `Blocked { IterationDidNotConverge }` with a three-button picker:

- **Raise threshold** — opens an Amendment dialog so you can edit the convergence threshold and re-run from `iterate_gate`.
- **Accept best iteration** — picks the iteration whose `iterate.best_selector` field ranks highest (or the last iteration if `best_selector` is unset). Downstream `validate_<stage>` runs on the chosen pass.
- **Abort task** — marks the stage failed; downstream slice doesn't run.

**Artifacts.** Each iteration's outputs land at `runtime/outputs/<stage>_iter_N/`; the chosen / converged result aliases to `runtime/outputs/<stage>/`. The agent retains every iteration's outputs so you can compare passes after the fact.

**Determinism caveat.** Iteration count is data-dependent — same atom + same data = same number of passes (deterministic) — but two different intakes can produce DAGs with different `_iter_N` task counts. The byte-reproducibility contract holds because both expansions are deterministic given the inputs.

---

## 9. Runtime artifact index

Files inside an emitted package that you may need to reference. All paths are relative to the emitted package root.

| Path | Purpose | Written when | Typical SME action |
|---|---|---|---|
| `WORKFLOW.json` | The DAG state: which tasks have run, which are ready, which are blocked. | Continuously — every harness iteration rewrites it. | Don't edit by hand; the Plan tab visualizes this. |
| `runtime/intake-conversation.jsonl` | Full chat transcript — every user turn, assistant turn, tool call. | Appended per turn. | Read when you need to remember what you said three weeks ago. |
| `runtime/decisions.jsonl` | Typed audit log of every SME click + LLM-driven mutation. | Appended at every Accept / Reject / Unblock / Branch / Amendment. | Read for audit / reproducibility questions. |
| `runtime/outputs/<task_id>/` | The working directory of a specific task — logs, intermediate files, decision records. | During task execution. | Linked from result cards — open in the `TaskLogDrawer` when you click a task. |
| `runtime/outputs/<task_id>/decision.json` | The agent's best-practice scoring record for a `discover_*` task. | When a discovery task picks a method. | Referenced from `awaiting_sme_selection` blockers — the UI reads it to render the radio-button list. |
| `results/tables/*.tsv` | Final result tables (DE results, cell-type counts, enrichment scores, etc.). | After the relevant stage completes. | Download or open in a spreadsheet — this is the "real" result. |
| `results/figures/<task_id>/` | PNG / SVG figures per task. | After the task completes. | Viewed in the Figures tab or inline on result cards. |
| `results/narrative/` | Per-stage prose reports. | After the reporting stage completes. | Read as the final writeup; checked by claim verification against `results/tables/`. |
| `ro-crate-metadata.json` | RO-Crate manifest — the formal provenance record. | At emission + every amendment / branch. | Reference for reproducibility; the `prov:wasDerivedFrom` edges link lineages. |
| `intake-facts.json` | Structured intake facts distilled from your prose during intake (modality, sample count, organism, comparison axis, deliverables). | Once at emission. | The harness sizing policy reads this to pick instance types; support may ask you to attach it when diagnosing "why did the plan look that way?" |
| `container.json` | Container-image pins + runtime provenance for the executor (so the run can be reproduced later with the exact tool versions). | Once at emission. | Don't edit. Attach when reporting a reproducibility bug. |
| `runtime/outputs/<task_id>/LOG.jsonl` | Per-task structured event log (stdout/stderr distilled into typed entries). | Appended during task execution. | Open from the TaskLogDrawer when you click a task; support uses this to trace crashes. |
| `runtime/cross_version_diff.json` | Row-level concordance report comparing this emitted package against its parent. Only written when `SessionLineage` has a parent (i.e., this package is the result of an Amendment or Branch). | Once at each amend / branch re-emission. | Read from the "Compare to previous version" link on result cards, or via `GET /api/chat/session/:id/cross-version-diff`. |
| `amendment-lineage.json` | The chain of amendments this package descended from (list of `{parent_session_id, parent_package, amended_stage, timestamp}`). | Only written on amendment re-emissions. | Read when auditing "how did we get here?" across many amendments. |

The full index (with policies and interpretation rules) is in [`docs/config-reference.md`](docs/config-reference.md).

---

## 10. Decision log variants

`runtime/decisions.jsonl` is an append-only audit trail of every high-leverage checkpoint. Each line is one `DecisionRecord` with a typed `decision` field — closed taxonomy whose authoritative variant count comes from the live enum at [`crates/core/src/decision_log.rs`](crates/core/src/decision_log.rs).

| `kind` | Fired when | Payload |
|---|---|---|
| `confirm` | The SME clicked **Confirm** on the summary card. Gates `emit_package`. | (none) |
| `reject` | The SME clicked **Make corrections**. | (none) |
| `unblock` | The SME cleared a blocker from the `BlockerCard`. | (none) |
| `branch` | A new branched session was forked from this one. | `child_session_id` |
| `emit_package` | The compiler emitted a package to disk. | `output_dir` (absolute path) |
| `amend_stage` | A stage's method was swapped post-emission. | `stage`, `method_prose` |
| `rerun_task` | A completed task was re-queued with its existing method. | `task_id`, optional `reason` |
| `select_sensitivity_winner` | The SME picked a winner from a sensitivity comparison. | `stage`, `winner` |
| `cross_version_diff` | The harness emitted a cross-version concordance report after an amend/branch re-emission. | `parent_package`, `child_package`, `overall_concordance`, `n_discordant` |
| `post_hoc_deviation` | An `amend_stage_method` or `rerun_task` targeted a stage declared prespecified in `SessionMode::Confirmatory`. A non-empty rationale is required. | `target_stage`, `prior_method`, `new_method`, `reason` |
| `auto_advanced` | `CheckpointMode::{Fast,Selective}` skipped an SME review gate that Gated mode would have paused on. | `stage`, `mode` |
| `applied_structured_decision` | The SME answered a structured-blocker decision point via the BlockerCard picker (or the equivalent REST endpoint). One record per decision point per answer round. | `task_id`, `decision_point_id`, `chosen` |
| `disposition_proposed` | The server ingested a new `sme_disposition.json` file (via an agent `disposition_proposed` progress event or a backfill scan). | `path`, `task_id`, `created_at`, `action_count` |
| `disposition_applied` | One action ran during `/dispositions/:path/apply`. `auto` flags the double-gate escape hatch (false on SME-clicked applies). | `path`, `action_index`, `action_kind`, `target_stage`, `outcome`, optional `error_reason`, `auto` |
| `disposition_rejected` | The SME rejected a disposition via `/dispositions/:path/reject`. | `path`, optional `rationale` |
| `undone_amendment` | The SME reversed a just-applied amendment via the Undo toast before the harness re-ran the invalidated forward slice. | `stage`, `reverted_to` |
| `budget_changed` | The SME set or cleared the session-level soft budget cap. | optional `prior_usd`, optional `new_usd` |
| `user_note` | The SME added a free-form audit note from the chat surface. | `note` |
| `set_intake_field` | The LLM tool loop captured a structured intake field. | `stage`, `field`, `value` |
| `set_intake_method` | The LLM tool loop captured an SME-named method choice. | `stage`, `method_prose` |
| `append_intake_prose` | The LLM tool loop appended free-form intake prose. | `prose` |

Every record also carries `timestamp` (ISO-8601 UTC), `session_id`, `actor` (`sme` / `llm` / `harness`), and optional `rationale` (free-form justification when the SME supplied one). Adding a new variant requires a plan amendment — the decision log is part of the reproducibility contract.

---

## 11. Model selection

The chat surface is backed by three Anthropic models. Which one responds to any given turn depends on policy, not on the prompt.

| Model | Role | Trigger |
|---|---|---|
| **Claude Sonnet 4.6** | Default conversational model | Every turn unless one of the escalation triggers below fires. |
| **Claude Opus 4.7** | Heavier-weight "careful" model | `careful_mode` on the session (set by the server for sensitive operations); the session is in `Blocked`; classifier confidence < 0.3 on the intake. |
| **Claude Haiku 4.5** | One-shot side calls | Auto-title generation after ≥ 3 non-system turns (gated by `SWFC_AUTO_TITLE=1`). Runs out-of-band via `ModelPolicy::for_side_call()` so it doesn't re-use the main conversation cache. |

You'll see Sonnet / Opus turn counts, token totals, and cost split in the Performance tab. Opus is ~5× the per-token cost of Sonnet, so a session that repeatedly escalates (e.g. many blockers, low-confidence intake) bills more. The escalation logic lives in [`crates/conversation/src/model_policy/mod.rs`](crates/conversation/src/model_policy/mod.rs).

---

# Part III — Reading the package + getting help

## 12. Cost, time, privacy, and what you can't do

### Cost

Two main cost streams accumulate during a session:

- **Chat.** Each turn of the conversation hits the Anthropic API. Typical ranges: **$0.01 – $0.50 for intake** (the intake phase is short by design); **$0.50 – $5.00** over the whole session including iteration turns. Opus escalations (careful mode, blockers, low confidence) cost ~5× the baseline Sonnet rate, so a blocker-heavy session can push into double digits.
- **Agent.** The actual analysis runs — each task the harness dispatches bills either your per-task subscription (the default) or the API (when `SWFC_AGENT_BILLING=api` is set). Order-of-magnitude ranges: **$1 – $10 for an IVD scRNA-seq run** on local or moderate-size cloud; **$10 – $50 for a whole-genome variant calling** run; **more** for GWAS and large metagenomics. A pilot sizing run (when `SWFC_PILOT_ENABLED=1`) projects the full-run cost before the real run kicks off, and a `pilot_oversize` blocker fires if the projection exceeds your `SWFC_AWS_COST_CEILING_USD`.

Optional third stream:

- **Scorer** (only when you click "Score transcript" on the Performance tab). Runs the rubric scorer once over the transcript. Small — a few cents per session.

The three streams appear in the Performance tab as `chat_cost_usd`, `agent_cost_usd`, `scorer_cost_usd`, summing to `total_cost_usd`.

### Time

- **Intake** — typically **5–15 minutes of conversation**. Short on the low end (simple RNA-seq with clear controls), longer for unusual modalities or ambiguous designs.
- **Execution** — depends entirely on modality and sample count. **IVD scRNA-seq (~47 libraries)**: 30–90 minutes end-to-end. **Whole-genome variant calling**: several hours. **Large GWAS or metagenomics**: can run overnight.

The Performance tab shows running cost and a rough ETA as more tasks complete.

### Privacy and data handling

Three data boundaries to keep in mind:

- **Chat prose** (your intake descriptions + the assistant's replies + any follow-up messages) is sent to the Anthropic API for processing. Anthropic's data-use policy applies: enterprise use does not train on prompts, but the data transits their infrastructure. Do not paste PHI, patient identifiers, or any other regulated identifiers into chat.
- **Sample data** (the actual FASTQ / BAM / VCF files, count matrices, phenotype tables) **stays on the harness host**. The chat surface never sees sample data; the agent running on the harness host does.
- **Results** (tables, figures, narrative) stay in the emitted package on the harness host. They only leave the host if you explicitly download them.

**For regulated data** (HIPAA, CLIA, clinical-trial PII, etc.), confirm with your institution's compliance officer before using the chat surface. The system has a `SWFC_CHAT_MODE=offline` kill switch that degrades gracefully with a mock backend — use it if chat is not acceptable for your environment, but note that no LLM-mediated features work in that mode.

**Network access.** By default the chat server binds `127.0.0.1` only, so the UI works on the same host as the server with no authentication. Operators who expose the server on a LAN (`SWFC_BIND_ADDR=0.0.0.0`) must set `SWFC_SERVER_AUTH_TOKEN` to a long random string; the UI reads the token from a `<meta name="swfc-auth-token">` tag in `ui/index.html`. The dev recipe is to set the token manually in `ui/index.html` for local LAN testing; production deployments template the index from the server at boot.

### What you can't do

A short list of things this tool is intentionally not designed to handle. The chat will politely decline — knowing them up front saves you from hunting.

- **Edit the plan by hand.** The task graph is derived from the taxonomy + your intake facts; there is no free-form plan editor. To change the plan, Revise or Branch.
- **Pin a specific tool version.** The execution agent picks tool versions at runtime, constrained by the emitted package's `best-practice-tool-registry.json`. If you need a specific version pinned, the tool-registry policy is the place to change it — that is a contributor-level edit.
- **Reload the browser mid-amendment without losing in-flight state.** The amendment itself persists to disk, but any in-flight assistant turn is dropped on reload. Wait for the amendment to complete before reloading.
- **Edit the emitted package directly.** All post-emission changes come through chat (Rerun / Revise / Branch / select winner). Hand-editing the package invalidates the audit trail and puts the session in an undefined state.
- **Share a session URL with a collaborator for live co-editing.** Not supported today. You can share the emitted package (which includes the full transcript) for async review.
- **Undo an Accept.** Accept writes the package to disk; there's no undo. Workarounds: branch from a pre-Accept point (if the session is still in `PendingConfirmation` in a parent), or start a new session.

---

## 13. Figure resolution paths

Every analysis stage is paired with a *plot affordance* — the system's record of how to render that stage's output. For established modalities (bulk RNA-seq, single-cell, etc.) the pairings are already registered; the figures appear automatically when the stage completes. For novel stages or custom output types the system uses one of four resolution paths:

- **Validated** — the stage's output type matches a registered catalog entry. You'll see the exact figure ids the catalog defines (e.g. `volcano`, `top_features_heatmap`).
- **Inherited** — the stage's output type is a recognized subtype of a registered type. The system renders the parent type's figures and labels them accordingly.
- **Generic** — the output type isn't in the catalog but the stage declares expected figure ids. The system renders the most appropriate standard primitives (bar chart, heatmap, scatter) for each id.
- **Proposed** — neither of the above applies. The result card shows a *"Describe a plot"* form where you can tell the system what kind of figure you expect. Your description is stored with the session and included in the next package re-emission so the rendering agent can prototype it.

You don't need to know which path applied; the Figures tab just shows what was produced. If a figure is missing after a stage completes, the system files a `MissingArtifact` blocker — click the Retry button on the blocker card.

---

## 14. Glossary

- **Amendment.** A session-level operation that swaps a method at one stage and reruns the downstream slice; produces a new emitted package with lineage pointing at the original. Trigger: Revise.
- **Branch.** A parallel copy of the current session, created from any point, with its own audit trail. Trigger: the Branch button on `BranchFromHereCard`.
- **Claim boundary.** A plain-English statement of what the analysis does and does not support — e.g., "statistical patterns, not causal claims." Every modality's taxonomy declares one; the assistant restates it in the plan summary before you Accept.
- **Discovery task.** A task prefixed `discover_*` (e.g., `discover_normalization`) — the agent runs best-practice scoring to pick a method for that stage. Always produces a `decision.json`.
- **Emit.** The moment the system writes a self-contained execution package to disk. Triggered when you click **Accept** on the confirmation card; gated by the `emit_package` tool which returns `PreconditionFailure` unless the session's `user_confirmed` flag is true.
- **Emitted package.** The self-contained RO-Crate directory that gets written when you Accept. Contains the DAG, policies, intake facts, and (post-execution) all results.
- **Intake.** The conversational phase where you describe your analysis in prose. The system distills your prose into a small set of structured **intake facts** (modality, sample count, organism, comparison axis, deliverables) and uses them to build the plan. Intake ends when you accept the confirmation card.
- **Intake facts.** The small, typed dictionary the classifier extracts from your prose during intake. Persisted to `intake-facts.json` in the emitted package and consumed by the harness sizing policy at execution time.
- **Lineage.** The `prov:wasDerivedFrom` chain recording how an amendment package descends from its parent. Visible in the History tab as a graph.
- **Archetype.** The composer fast-path scaffold for an analysis shape (`bulk_rnaseq_de`, `single_cell_de`, `clinical_trial_analysis`, …). One YAML per archetype under `config/archetypes/`; the planner uses it as a seed candidate when the classifier produces a goal.
- **Atom.** A typed `(operation × input × output)` triple — the unit of composition the composer reasons over. One YAML per atom under `config/stage-atoms/`.
- **Modality.** The high-level assay category — `single_cell_rnaseq`, `bulk_rnaseq`, `proteomics`, `variant_calling`, etc. Drives the archetype selected by the composer.
- **Pilot.** A small-scale right-sizing run that projects full-run cost before the real run kicks off. Currently gated behind `SWFC_PILOT_ENABLED=1`.
- **Project class.** A higher-level classification over and above modality — `research`, `clinical_trial`, `time_series_forecast`. Selects project-class-routable archetypes (`clinical_trial_analysis`, `time_series_forecast`) and drives which extra fields the confirmation card surfaces.
- **RO-Crate.** An open standard for packaging research data and computation with provenance metadata. Every emitted package is a valid RO-Crate: the `ro-crate-metadata.json` manifest at the package root declares each input artifact, each output artifact, each task, and the `prov:wasDerivedFrom` edges connecting them. Lets external tools verify reproducibility without custom tooling.
- **Sensitivity analysis.** A stage class (`sensitivity_comparison`) where multiple methods run in parallel and the SME picks the winner. Drives `select_sensitivity_winner`.
- **Stage.** An internal name for a node in the DAG (e.g., `discover_normalization`, `validate_qc`, `select_aligner`). You normally see the humanized label ("Normalization") in the UI, not the raw ID.
- **Winner.** The method the SME selects in a sensitivity comparison, or the agent's top pick in a best-practice-scored discovery task. Drives all downstream work.

---

## 15. Troubleshooting

| Symptom | Likely cause | Next action |
|---|---|---|
| The assistant doesn't reply after my first message. | The LLM API key isn't configured (server defaults to an empty mock backend). | Contact Alan / the ops team. The server has a kill switch (`SWFC_CHAT_MODE=offline`) that explicitly acknowledges this state. |
| A result card shows a red "mismatch" verification banner. | Claim verification found a disagreement between the narrative and the table. | Open the cited table, check the flagged row, decide which side is right, then Revise narrative or Revise method. Don't click past — the session is blocked. |
| I clicked Accept but nothing seems to be running. | Accept freezes the scope; Start execution kicks off the run. Two separate buttons. | Click the Start execution button on the plan view. |
| The Progress tab shows the same task "started" three times. | The system is batching bursts of small events into one turn. | Wait a few seconds; you'll see a synthesis message ("Finished: ..."). |
| A blocker card has an unfamiliar kind name. | New blocker variant added in a recent release. | Check §5 above; if still missing, ask. |
| "Unexpectedly high cost projection" | Pilot sizing projected a full run above your cost ceiling. | Either lower the ceiling (for a smaller analysis), rescope the inputs, or Accept the overage from the blocker card. |
| Branching doesn't show up as a separate session. | The session list UI scopes to the current lineage by default. | Check the History tab for the parent / child graph. |
| Session went quiet for hours. | A task is running, but took longer than the 8-second "still thinking" indicator threshold. | Switch to Progress tab; the task is likely running (check for a "Started: …" line with no "Finished" yet). Stall detection fires after ~15 minutes. |
| The narrative references a file path I can't open. | Agent wrote an internal runtime path into narrative prose. | The UI sanitizer *should* have caught this. If it leaks, report a bug — file + line where you saw it. |
| I want to undo an Accept. | Accepts aren't directly undo-able; the package is already written. | Branch from the pre-Accept state (if the session was in `PendingConfirmation` recently) or start a new session. |

---

## 16. Getting help

- For how to run the system locally, configure it, or contribute code, see [`README.md`](README.md) and [`CONTRIBUTING.md`](CONTRIBUTING.md).
- For deeper operator and contributor references — API routes, configuration files, env vars, remote compute, container runtime, git provenance — see [`docs/`](docs/).
- For questions that this guide doesn't cover, ask Alan (alan@hueb.org) or the Scripps workflow team.
