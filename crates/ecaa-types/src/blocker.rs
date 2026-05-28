//! Closed taxonomy of reasons a task or chat session can land in a
//! Blocked state.
//!
//! The enum uses internally-tagged serde (`#[serde(tag = "kind")]`) so
//! the on-wire JSON shape is flat — e.g.
//! `{"kind":"data_shape_mismatch",... }` — which is forward-compatible
//! with adding new payload fields per variant.
//!
//! Re-exported from `scripps_workflow_core::blocker` for backward
//! compatibility with existing call sites. Variant payload types that
//! live elsewhere in this crate (`NetworkPolicy`, `SandboxRequirement`,
//! `ToolErrorEnvelope`) are pulled in from the sibling modules.

use crate::atom::{NetworkPolicy, SandboxRequirement};
use crate::error_envelope::ToolErrorEnvelope;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Structured failure cause for [`BlockerKind::ValidationFailed`]. Phase A
/// of the literature-atom plan — extends `ValidationFailed` with an
/// optional typed payload for validators that produce structured failure
/// reasons (e.g. literature-row-anchored errors). `None` preserves the
/// legacy path where the `check` + `message` string pair is the only
/// failure information.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "cause_kind", rename_all = "snake_case")]
pub enum ValidationFailureCause {
    /// A literature-validator obligation failed on a specific row of a
    /// literature artifact CSV (prior_claims_matrix.csv or
    /// claims_evidence_matrix.csv). The verifier embeds the row index and
    /// the structured failure kind so the SME-facing BlockerCard can
    /// dispatch the right recovery affordance.
    LiteratureClaim {
        row_index: u64,
        artifact: String,
        kind: LiteratureClaimFailureKind,
    },
}

/// Closed set of literature-claim validator failure kinds. Maps 1:1 onto
/// the validator obligations in `validation_obligations.rs::literature_obligations`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum LiteratureClaimFailureKind {
    PmidMalformed,
    PmidNotFound,
    EvidenceArtifactMissing,
    QuoteNotInSource,
    /// The evidence_quote was found in the source text but at a byte
    /// offset substantially different from the row's declared
    /// evidence_quote_offset (tolerance: 1024 bytes for normalization shifts).
    QuoteOffsetWrong,
    RedistributableTagInconsistent,
    FindingIdOrphan,
    InvalidConcordanceFlag,
}

/// Why a task or session is blocked. Closed taxonomy (47 variants;
/// see test `all_variants_roundtrip_serde` for the canonical count and
/// the `BlockerKind::COUNT` compile-time gate in
/// `crates/core/tests/blocker_variant_count.rs`).
///
/// Internally-tagged with `kind` so the serialized JSON is flat:
///
/// ```json
/// { "kind": "data_shape_mismatch", "expected": "matrix", "actual": "list" }
/// ```
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, TS, strum::EnumCount, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum BlockerKind {
    /// Input had the wrong shape for the downstream consumer
    /// (e.g. expected a matrix, got a nested list).
    DataShapeMismatch { expected: String, actual: String },

    /// A named validation check failed. `check` is the check identifier;
    /// `message` is the human-readable reason. `cause` carries a typed
    /// failure payload for validators that produce one (literature claims,
    /// etc.); legacy string-only validators set `None`.
    ValidationFailed {
        check: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<ValidationFailureCause>,
    },

    /// A required metric fell below its threshold.
    MetricBelowThreshold {
        metric: String,
        threshold: f64,
        actual: f64,
    },

    /// Upstream dependency (task id) is missing or not yet complete.
    MissingInput { dependency: String },

    /// The execution agent errored out. `message` is agent-supplied text.
    AgentError { message: String },

    /// The host (harness / orchestrator / compiler) errored out.
    HostError { message: String },

    /// The SME must pick from a set of candidates for a named stage.
    AwaitingSmeSelection {
        stage_id: String,
        candidates: Vec<String>,
    },

    /// The pre-flight pilot projected a full-run cost that exceeds the
    /// operator-configured ceiling. The SME decides whether to raise
    /// the ceiling, reduce the pilot sample, or abort.
    PilotOversize {
        projected_usd: f64,
        ceiling_usd: f64,
    },

    /// A running task triggered a stall signal (CPU starvation, memory
    /// pressure, idle GPU, runtime overrun). Carries the structured
    /// signal and the recommended recovery action.
    Stalled {
        task_id: String,
        signal: StallSignalWire,
        suggested_action: StallAction,
    },

    /// The harness detected that a required assertion in
    /// `policies/validation-contract.json` was unsatisfied. `contract_id`
    /// identifies the contract file; `assertion_ids` lists the specific
    /// required assertions that didn't pass. The UI renders a
    /// contract-violation BlockerCard that cites each id so the SME
    /// (or the agent on re-entry) can remediate each one explicitly.
    ContractViolation {
        contract_id: String,
        assertion_ids: Vec<String>,
    },

    /// The SME-pinned method cannot execute because a required runtime
    /// capability (R package, Python library, system binary) is
    /// missing. `recommended_substitute` names the same-algorithm port
    /// the agent found available, if any — surfaced as the default
    /// option in the BlockerCard's structured picker. Prevents the
    /// DESeq2→pydeseq2 / fgsea→gseapy substitution flows from collapsing
    /// to `DataShapeMismatch` and forcing curl-based SME rescue.
    RuntimeCapabilityMissing {
        sme_pinned_method: String,
        missing_capability: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        recommended_substitute: Option<String>,
    },

    /// Generic "the agent wrote `blocker.json` with structured
    /// decision points; see that file for the full question set" kind.
    /// Covers the agent vocabulary `awaiting_sme_input`,
    /// `awaiting_sme_approval`, `env_capability_skip`,
    /// `runtime_substitution`, and anything the mapper doesn't
    /// recognise — the UI fetches `decision_points_path` and renders a
    /// per-decision radio picker. `summary` is a one-line human
    /// summary for the header chip and BlockerCard title.
    AwaitingStructuredDecision {
        task_id: String,
        decision_points_path: String,
        summary: String,
    },

    /// The agent completed a discovery scoring pass and is asking the
    /// SME to confirm the top candidate (optionally overriding with a
    /// runner-up). Differs from `AwaitingSmeSelection` in that the
    /// top candidate is recommended rather than neutral — picker shows
    /// the top + runner-ups with the top pre-selected. Emitted by the
    /// agent as `awaiting_sme_approval`.
    AwaitingSmeApproval {
        stage_id: String,
        top_candidate: String,
        runner_ups: Vec<String>,
    },

    /// The silent-completion guard detected that the agent marked a
    /// task `Completed` but one or more required artifacts are missing,
    /// empty, or below their declared `min_size_bytes`. UI BlockerCard
    /// offers a single-click Rerun with the missing artifact names
    /// shown as a hint.
    MissingArtifact {
        task_id: String,
        /// Relative paths of the artifacts the guard couldn't verify.
        missing_paths: Vec<String>,
    },

    /// The harness observed that every Running task's heartbeat file
    /// is older than the configured threshold
    /// (`SWFC_TASK_HEARTBEAT_STALL_SECS`). Distinct from
    /// `BlockerKind::Stalled` (which is a CPU / memory / GPU / runtime
    /// signal from the stall monitor). UI BlockerCard offers Rerun.
    HeartbeatStalled {
        task_id: String,
        /// Seconds since the last observed heartbeat touch.
        last_heartbeat_secs_ago: u64,
    },

    /// On harness startup the dispatch WAL recovery pass found a task
    /// still in `Running` state that was dispatched by a prior harness
    /// process (different `harness_run_id`). The task was re-blocked so
    /// the SME can rerun it deterministically instead of relying on the
    /// stale-timeout heuristic.
    OrphanedByCrash {
        task_id: String,
        prior_harness_run_id: String,
        /// RFC 3339 timestamp of the dispatch that was orphaned.
        last_dispatch_at: String,
    },

    /// The agent / executor captured a structured failure from the
    /// underlying processing library or binary (OOM, MemoryError,
    /// DESeq2 ImportError, STAR segfault, SLURM wallclock-exceeded,
    /// AWS spot-interrupted task, etc.). The envelope crosses every
    /// backend uniformly; the chat surface routes it to a typed
    /// remediation card via the `remediation_proposer` side-call.
    /// Distinct from `AgentError { message }` — the latter remains
    /// the fallback when capture didn't produce an envelope.
    ToolError { envelope: Box<ToolErrorEnvelope> },

    /// At task start the runtime container's resolved digest didn't
    /// match the digest pinned in `WORKFLOW.json` /
    /// `policies/container.json`. Distinct from `ContainerPullFailed`
    /// (which is a registry-side failure) — a pull succeeded but
    /// the resulting image's `RepoDigests` didn't agree with the
    /// emit-time pin. Indicates registry tampering, a re-tagged
    /// floating tag, or a stale local cache. SME affordance: rerun
    /// after pruning the cache (clears the local image), or accept
    /// the new digest by re-emitting the package.
    ImageDigestMismatch {
        expected_digest: String,
        actual_digest: String,
    },

    /// The container runtime (Apptainer / Docker / podman) failed to
    /// pull the image from the registry. `reason` carries the
    /// runtime-supplied failure text — typical causes are HTTP 401
    /// (unauthorised), HTTP 404 (tag/digest not found), or transient
    /// network failure. SME affordance: retry, swap the image, or
    /// configure registry credentials.
    ContainerPullFailed { image: String, reason: String },

    /// The pull succeeded but the runtime couldn't start the
    /// container (missing GPU driver on the host, kernel ABI
    /// mismatch, unsupported OS, missing capabilities). `reason`
    /// is the runtime's exec failure text. SME affordance: retry
    /// on a different host, fall back to host-env via
    /// `SWFC_DISABLE_CONTAINERS=1`, or pivot to a CPU image if the
    /// failure is GPU-related.
    ContainerStartFailed { image: String, reason: String },

    /// The container runtime binary (`apptainer`, `docker`,
    /// `podman`) isn't on the host's PATH. The harness probes
    /// runtimes in priority order; this kind fires when none are
    /// available. SME affordance: install a runtime, set
    /// `SWFC_CONTAINER_RUNTIME=<name>` to point at a non-default
    /// install path, or set `SWFC_DISABLE_CONTAINERS=1` to fall
    /// through to the host environment.
    RuntimeMissing { runtime: String },

    /// The Syft SBOM emit step at task completion failed. The task
    /// itself completed successfully but the supply-chain attestation
    /// couldn't be written. `reason` is the syft binary's failure
    /// text. SME affordance: rerun the SBOM emit standalone, or set
    /// `SWFC_SBOM_EMIT=0` to skip on subsequent tasks.
    SbomEmissionFailed { reason: String },

    /// The task attempted network egress while running under a
    /// container with `network: none` (typically the
    /// `clinical_trial` archetype per ADR 0028). `policy` is the
    /// declared network policy, `attempted` is the destination the
    /// task tried to reach. SME affordance: amend the task's
    /// container `network` policy to `bridge`, or remove the
    /// network-dependent step from the analysis.
    NetworkPolicyViolation { policy: String, attempted: String },

    /// The per-session container cache mount detected on-disk
    /// corruption (truncated layer, checksum mismatch, OverlayFS
    /// inconsistency). `path` is the cache mount root. SME
    /// affordance: prune the cache via the
    /// `prune-container-cache` action, then rerun.
    ContainerCacheCorrupted { path: String },

    /// The SLURM scheduler reported `OUT_OF_MEMORY` (or AWS reported
    /// SIGKILL with peak RSS at the cgroup limit) for the running task.
    /// Distinct from `Stalled { signal: MemoryPressure }` (which is a
    /// rate-of-change signal from the stall monitor that fires *before*
    /// the kernel kills the process). This kind fires *after* the kill.
    /// `peak_memory_mb` is the observed peak from sacct/cgroup; `limit_mb`
    /// is the cap that was breached. SME affordance: rerun on a larger
    /// resource class (the executor offers a one-click resize action).
    MemoryExhausted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        peak_memory_mb: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        limit_mb: Option<u64>,
    },

    /// The SLURM scheduler reported `TIMEOUT` (the task hit the
    /// `--time=` wallclock cap) or AWS reported the task exceeded its
    /// `expected_secs * SWFC_RUNTIME_MULTIPLIER` budget. Distinct from
    /// `Stalled { signal: RuntimeOverExpected }` which fires *during*
    /// runtime as a heads-up — this kind fires *after* the scheduler
    /// has already killed the job. `wallclock_secs` is the elapsed time
    /// at termination; `time_limit_secs` is the cap. SME affordance:
    /// rerun with a longer `--time=` (the executor offers a one-click
    /// extend-and-rerun action).
    TimeExceeded {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        wallclock_secs: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        time_limit_secs: Option<u64>,
    },

    /// Decision-log replay or session-envelope upcasting
    /// hit a corrupted record and the load was running in permissive
    /// mode (the default outside CI/test). The triggering record was
    /// skipped and appended to `runtime/sessions.errors.jsonl` /
    /// `runtime/decisions.errors.jsonl` for operator review; this
    /// kind surfaces in the BlockerCard so the SME knows the session
    /// they're seeing isn't a faithful replay. `event_id` identifies
    /// the offending record by its `record_id`/`event_id`;
    /// `schema_version` is the version of the source record;
    /// `reason` is the parse-error string from serde / the upcast
    /// branch. Distinct from `HostError` (load-side, not run-side).
    ReplayCorruption {
        event_id: String,
        schema_version: u32,
        reason: String,
    },

    /// Emit-time image-digest resolution failed. The
    /// composer attempted to pin every `ContainerSpec`'s digest before
    /// WORKFLOW.json write (so the package is byte-deterministic
    /// across runs) but couldn't reach the registry, the requested
    /// `image:tag` returned 401/404, or the registry response wasn't
    /// a valid digest. `image` and `tag` identify the unresolved
    /// reference; `reason` is the resolver's diagnostic. SME
    /// affordance: retry once registry is reachable (transient), pin
    /// a different image:tag (mistyped / removed), or set
    /// `SWFC_DISABLE_CONTAINERS=1` to fall through to host-mode.
    ImageDigestUnresolved {
        image: String,
        tag: String,
        reason: String,
    },

    /// Composer (backward-chain) cannot produce a valid DAG for the
    /// SME's `(IntakeFacts, GoalSpec)` pair. Three orthogonal failure modes reported together so the
    /// SME sees the full picture, not just the first wall-hit:
    ///
    /// - `missing_inputs`: EDAM-typed declarations the composer
    ///   couldn't satisfy from any atom in the registry. Each entry
    ///   is the matching atom port the composer was looking for, e.g.
    ///   `data:2044 (Sequence) format:1930 (FASTQ)`. Galaxy LLM Hub
    ///   pattern (Oct 2025): name the ontology terms not the prose.
    /// - `unreachable_goal`: the GoalSpec's `(edam_data, edam_format)`
    ///   has no producing atom in the current registry — typically
    ///   means the SME asked for an artifact shape we don't compose
    ///   yet. None when the goal IS reachable but a different
    ///   constraint blocks (input or exclusion).
    /// - `excluded_paths`: atoms the backward chain considered but
    ///   filtered out via their `excludes:` CEL expressions. Surfaced
    ///   so the SME can see what the composer ruled out (and why)
    ///   instead of a silent dead end. Each entry pairs the atom id
    ///   with the exclusion's CEL source.
    ///
    /// The UI `CompositionInfeasibleCard` (S7.7) renders all three
    /// blocks with "Try adding…" affordances tied to ontology terms.
    CompositionInfeasible {
        missing_inputs: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        unreachable_goal: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        excluded_paths: Vec<ExcludedPath>,
    },

    /// Container exited with a non-zero status. The
    /// agent process inside the container returned an error; the
    /// harness captured `exit_code` from the exec wrapper. When the
    /// kernel OOM-killed the container, `oom_killed: true` (cgroup
    /// `memory.events.local::oom_kill` ≥ 1 on cgroup v2; dmesg-scrape
    /// fallback on cgroup v1). Distinct from `MemoryExhausted` (which
    /// is the harness-side stall-monitor classification of a SLURM
    /// `OUT_OF_MEMORY` state) because cgroup-OOM-kill happens inside
    /// the agent's lifetime and yields a partial output set the SME
    /// can still inspect. SME affordance: re-run with a larger memory
    /// cap (`SWFC_AGENT_MEMORY_CAP_GB`), or amend the stage method to
    /// a less memory-hungry alternative.
    ContainerExitedAbnormally {
        exit_code: i32,
        #[serde(default)]
        oom_killed: bool,
    },

    /// SLURM partition lacks the container runtime the atom's
    /// `preferred_container` requires. The atom
    /// declares (e.g.) `apptainer-1.4`, the partition's
    /// `container_runtimes:` entry in `slurm-mapping.yaml` lists what
    /// the site supports, and `slurm/sbatch.rs::validate_submission`
    /// refuses to submit when there's no overlap. `partition` is the
    /// SLURM partition name; `required` is the atom's runtime
    /// declaration; `available` is the site's capability list. SME
    /// affordance: pick a different partition (when one exists with
    /// the runtime), set `SWFC_SLURM_NATIVE_CONTAINER=1` to use SLURM
    /// 25.11's `--container` directive when applicable, or amend the
    /// atom to drop the container requirement.
    SlurmRuntimeUnavailable {
        partition: String,
        required: String,
        available: Vec<String>,
    },

    /// An iterate-until atom hit `max_iterations`
    /// without satisfying the convergence rule for the required
    /// `consecutive_iterations`. Surfaces in the chat-side
    /// BlockerCard with a 3-button picker (raise threshold / accept
    /// best so far / abort) per the §S10.5 acceptance row. Distinct
    /// from `Stalled` (which is a CPU/memory/runtime signal — the
    /// iteration is making progress, just not converging) and from
    /// `MetricBelowThreshold` (which fires on a single failed
    /// validation gate). `task_id` is the iterate atom's task id;
    /// `iterations_run` is how many passes ran; `last_metric` is the
    /// final value of the convergence metric; `threshold` is the
    /// configured target. The agent expansion (S10.4) is responsible
    /// for surfacing this — composer treats iterate atoms as single
    /// nodes (S10.6) and never sees the runtime-expanded chain.
    IterationDidNotConverge {
        task_id: String,
        iterations_run: u32,
        last_metric: f64,
        threshold: f64,
    },

    /// The container-aware orphan reaper observed a
    /// stale heartbeat (older than `SWFC_TASK_HEARTBEAT_STALL_SECS`)
    /// while the SSM/SSH probe found the container itself still
    /// alive (`docker ps --filter label=swfc-task=<id>` returned a
    /// row, or `apptainer instance list` showed the named instance).
    /// Distinct from `HeartbeatStalled` — that variant fires when the
    /// heartbeat is stale and we have no container-level signal at
    /// all (legacy host-mode or pre-S15.22 task). `ContainerHung`
    /// fires when we know the host instance is healthy and only the
    /// in-container agent is wedged, so the recovery affordance is
    /// "reap container only and re-dispatch on the same host" rather
    /// than "tear the host down". `task_id` is the wedged task;
    /// `container_id` is the runtime's id (empty for apptainer);
    /// `runtime` is the runtime that reported the alive container
    /// (`docker` / `podman` / `apptainer`);
    /// `last_heartbeat_secs_ago` matches the `HeartbeatStalled` field
    /// so the UI can render the same age chip.
    ContainerHung {
        task_id: String,
        container_id: String,
        runtime: String,
        last_heartbeat_secs_ago: u64,
    },

    /// The harness rejected a generated-code task before dispatch
    /// because its `TaskNode.implementation` did not satisfy the
    /// active `SandboxPolicy`. The `refusals` payload
    /// is the typed list returned by
    /// `harness::sandbox_enforcer::pre_dispatch_check`.
    SandboxRefused { refusals: Vec<SandboxRefusalRecord> },

    /// V3 one of the six non-monotonic lifecycle edges
    /// from design §7 fired during `rebuild_dag` and the
    /// session-scoped adjudication queue gained an entry awaiting
    /// SME or operator review. `queue_entry_id` matches an entry
    /// on `Session::adjudication_queue`; `transition_kind` is the
    /// `LifecycleTransition::kind()` discriminator.
    AdjudicationRequired {
        queue_entry_id: String,
        transition_kind: String,
    },

    /// Atom declared sandbox != None but the chosen
    /// executor cannot provide it. Dispatch-time refusal (vs.
    /// `SandboxRefused` which is a runtime enforcement event).
    SandboxRequired {
        atom_id: String,
        requested: SandboxRequirement,
        available: SandboxRequirement,
    },

    /// Atom's safety.network is incompatible with the
    /// chosen executor's effective network policy (e.g. Network-level
    /// atom on an egress-denied SLURM node). Dispatch-time refusal
    /// (vs. `NetworkPolicyViolation` which is a runtime event).
    NetworkPolicyMismatch {
        atom_id: String,
        atom_network: NetworkPolicy,
        executor_network: NetworkPolicy,
    },

    /// Install proxy denied a package install at task
    /// runtime per the atom's provisioning policy. Runtime event, not
    /// dispatch-time.
    ProvisioningDenied {
        atom_id: String,
        package: String,
        registry: String,
    },

    /// A config manifest (modality / archetype) carries a
    /// `schema_version` the loader doesn't know how to handle, and the
    /// `MigrationRegistry` has no migrator chain covering the gap.
    /// `config_kind` is the IR-type key registered with the migration
    /// registry (e.g. `"modality_config"`, `"archetype_config"`);
    /// `expected` is the loader's current schema version;
    /// `found` is the version declared on disk. UI dispatches the
    /// recovery affordance: rerun against an upgraded loader, or
    /// migrate the on-disk manifest.
    SchemaVersionMismatch {
        config_kind: String,
        expected: String,
        found: String,
    },

    /// A task input port carries `safety.controlled_access == true` and
    /// the chosen executor would route it through an LLM agent (Local /
    /// Aws / Slurm with the Claude agent wrapper). Dispatch is refused
    /// because controlled-access data must not be forwarded to a
    /// third-party LLM inference endpoint. SME affordance: declassify
    /// the data source via an institutional data-sharing agreement, or
    /// switch to a host-mode executor (`SWFC_EXECUTOR_MODE=local` +
    /// `SWFC_DISABLE_CONTAINERS=1`) that does not call an LLM service.
    ControlledAccessViolation {
        task_id: String,
        port_name: String,
        attempted_call: String,
    },

    /// The aggregate byte total of `runtime/outputs/<task_id>/` exceeded
    /// the operator-configured cap (`SWFC_TASK_OUTPUT_MAX_MB`, default
    /// 5120 = 5 GiB). The harness refused to merge the task's
    /// `state.patch.json` and blocked the task so disk exhaustion cannot
    /// spread silently across the package root.
    ///
    /// This is the aggregate-size complement to `swfc_io`'s per-file cap
    /// (100 MiB): the per-file cap prevents OOM from a single giant blob;
    /// this cap prevents disk exhaustion from many medium-sized blobs
    /// (e.g. hundreds of per-sample CSV/parquet files).
    ///
    /// SME affordance: inspect `runtime/outputs/<task_id>/` for large
    /// unexpected files, clean up, and rerun with a higher cap or a
    /// method that produces smaller intermediate outputs.
    OutputSizeExceeded {
        task_id: String,
        observed_bytes: u64,
        threshold_bytes: u64,
    },

    /// The harness found a `state.patch.json` for the task but the file
    /// failed JSON parsing. The malformed bytes have been renamed to
    /// `state.patch.json.rejected-<compact_timestamp>` in the same
    /// directory so the agent's intent is preserved for post-incident
    /// review and the `remediation_proposer` side-call. `rejected_path`
    /// is the full path of the renamed file; `parse_error` is the serde
    /// diagnostic. SME affordance: inspect the rejected file, fix the
    /// agent's output serializer, and rerun the task.
    PatchUnparseable {
        task_id: String,
        rejected_path: String,
        parse_error: String,
    },

    /// The harness's local clock differs from the server's clock by
    /// more than `SWFC_CLOCK_SKEW_THRESHOLD_SECS` (default 60).
    /// Clock skew corrupts WAL timeout_at math and can trigger false
    /// orphan flags. Dispatch is refused until the clocks are brought
    /// into agreement. `observed_secs` is the absolute difference
    /// between `client_now` (harness) and `server_now` (server);
    /// `threshold_secs` is the configured ceiling. SME affordance:
    /// correct the host or server clock via NTP, or raise the threshold
    /// with `SWFC_CLOCK_SKEW_THRESHOLD_SECS` if the skew is acceptable.
    ClockSkew {
        /// Absolute difference between harness clock and server clock, in seconds.
        observed_secs: i64,
        /// Configured ceiling (from `SWFC_CLOCK_SKEW_THRESHOLD_SECS`).
        threshold_secs: u64,
    },

    /// The wall-clock watchdog detected that a Running task has been
    /// executing for longer than `task.expected_wall_seconds *
    /// SWFC_WATCHDOG_MULTIPLIER` (default 6×), or that the dispatch
    /// record's `timeout_at` timestamp has elapsed. This fires
    /// irrespective of CPU / memory / heartbeat signals — it catches
    /// CPU-bound infinite loops that produce a fresh heartbeat touch
    /// but never make overall progress. `observed_secs` is the elapsed
    /// wall time at detection; `threshold_secs` is the computed budget.
    /// SME affordance: abort the task, inspect the agent's output for
    /// an infinite loop, then rerun or amend the stage method.
    WallClockExceeded {
        task_id: String,
        observed_secs: u64,
        threshold_secs: u64,
    },

    /// The harness soft-cancelled a running task because the
    /// session transitioned to `Amending { target_stage, invalidated_tasks }`
    /// and this task's id was in the `invalidated_tasks` list. The
    /// harness sent SIGTERM (10s grace) then SIGKILL; for AWS it issued
    /// `ssm cancel-command`; for SLURM it ran `scancel`. The task is now
    /// Blocked so the SME's amend-and-re-emit flow can requeue it when
    /// the amendment package is ready. `target_stage` is the stage the
    /// SME amended; SME affordance is simply to re-emit (the server
    /// transitions this blocker away when the new package is accepted).
    CancelledByAmendment {
        task_id: String,
        target_stage: String,
    },

    /// A git-provenance commit hook was dropped before executing because
    /// the `GitHookPool` queue was full (all `max_concurrent` slots
    /// occupied) or the hook exceeded its per-hook wall-clock timeout
    /// (30 s by default). The recovery-point timeline has a gap at the
    /// named `trigger` operation. Operators can recover by re-running
    /// the git commit manually against the package directory; the session
    /// state itself is unaffected. `trigger` is the operation name
    /// ("emit", "amend", "branch", …); `reason` is either
    /// `"pool_saturated"` or `"timeout_secs=<n>"`.
    ProvenanceCommitDropped { trigger: String, reason: String },

    /// Executor agent exceeded the per-task turn budget
    /// (`MAX_TURNS_PER_TASK`, default 40). Indicates probable runaway
    /// loop or genuine complexity needing operator review.
    TurnBudgetExceeded,
}

/// One entry on the `excluded_paths` list of
/// `BlockerKind::CompositionInfeasible`. Records an atom the
/// backward chain considered but excluded, plus the CEL expression
/// (verbatim from the atom's `excludes:` block) that matched. Lets
/// the SME see why a candidate was ruled out without re-running the
/// composer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ExcludedPath {
    pub atom_id: String,
    pub exclusion_cel: String,
}

/// Per-refusal record carried in `BlockerKind::SandboxRefused`.
/// Mirrors `crates/core/src/sandbox_policy.rs::SandboxRefusal` but with
/// stringified discriminator + reason so the UI dispatch table can
/// render without depending on the harness crate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct SandboxRefusalRecord {
    pub kind: String,
    pub detail: String,
}

/// Wire-compatible mirror of the harness-internal `StallSignal` enum.
/// The harness crate cannot be pulled into `core` (no tokio, no harness
/// deps), so stall signals cross the crate boundary in this flattened
/// shape. `#[serde(tag = "kind", rename_all = "snake_case")]` produces
/// `{"kind":"cpu_starvation","avg_cpu_pct":2.3,"window_mins":34}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StallSignalWire {
    /// CPU has averaged below the threshold for longer than the window.
    CpuStarvation { avg_cpu_pct: f32, window_mins: u64 },
    /// Memory pressure has sustained above the threshold.
    MemoryPressure { pct: f32, window_mins: u64 },
    /// A GPU-training task's GPU has been idle for longer than the
    /// configured window.
    GpuIdleDuringTraining { window_mins: u64 },
    /// The task has been running for longer than `expected_secs` × the
    /// configured multiplier.
    RuntimeOverExpected {
        actual_secs: u64,
        expected_secs: u64,
    },
}

/// What the stall monitor recommends the SME do. `BlockerCard` highlights
/// this as the default action; all three buttons are always offered.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum StallAction {
    /// Terminate the current task, provision a larger instance, and
    /// re-run from scratch.
    Resize,
    /// Terminate and retry on the same shape. Use when the stall is
    /// suspected to be a transient fault rather than an under-sizing.
    Retry,
    /// Terminate the task, mark it failed, do not re-run.
    Abort,
}

/// Context attached to a blocker. Separate from `BlockerKind` so that each
/// variant can be reasoned about in isolation while still carrying the
/// timestamp + optional recovery hints that every blocker needs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct BlockerContext {
    /// ISO 8601 timestamp when the blocker was created.
    pub timestamp: String,
    /// Optional free-form recovery hints for the SME or agent.
    pub recovery_hints: Option<String>,
}

/// One entry in the per-session queue of active blockers. `SessionState::Blocked` carries `Vec<BlockerEntry>` so a
/// second concurrent blocker for a different task is appended rather
/// than overwriting the first.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct BlockerEntry {
    /// Stable per-entry id used by the UI as a React key.
    #[ts(type = "string")]
    pub blocker_id: uuid::Uuid,
    /// Originating task id. Session-level blockers use `_session`.
    pub task_id: String,
    /// Typed taxonomy used by the UI to pick the recovery affordance.
    pub kind: BlockerKind,
    /// Human-readable summary for the card.
    pub message: String,
    /// Optional recovery hint for this specific entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub recovery_hint: Option<String>,
    /// UTC timestamp this entry was first appended.
    #[ts(type = "string")]
    pub at: chrono::DateTime<chrono::Utc>,
}

impl BlockerEntry {
    pub fn new(task_id: impl Into<String>, kind: BlockerKind, message: impl Into<String>) -> Self {
        Self {
            blocker_id: uuid::Uuid::new_v4(),
            task_id: task_id.into(),
            kind,
            message: message.into(),
            recovery_hint: None,
            at: chrono::Utc::now(),
        }
    }

    pub fn with_recovery_hint(mut self, hint: impl Into<String>) -> Self {
        let hint = hint.into();
        if !hint.is_empty() {
            self.recovery_hint = Some(hint);
        }
        self
    }
}
