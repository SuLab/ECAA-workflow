// Workspace targets Unix-only: agent wrappers shell out to bash, the
// harness uses POSIX `setsid()` + flock for process-group and
// session-lock semantics, and the SLURM/AWS executors assume POSIX
// shells on the remote side. Failing fast at compile time is friendlier
// than chasing CRLF and `fork()` surprises in a CI run.
#[cfg(not(unix))]
compile_error!("ecaa-workflow targets Unix-only (Linux + macOS). Windows is not supported.");

pub mod ablation;
pub mod adapter_registry;
// Per-task LLM-generated code capture. Written by agent-claude.sh to
// runtime/outputs/<task_id>/agent-code.json; served by the per-task
// result endpoint. Excluded from the byte-reproducibility baseline.
pub mod agent_code;
pub mod archetype;
pub mod archetype_registry;
pub mod archetype_slots;
pub mod assumption_policy;
pub mod atom;
pub mod atom_registry;
pub mod atom_safety;
// HMAC-signed audit-log rows; closes the sidecar-forgery threat.
pub mod audit_writer;
pub mod backend_emitters;
pub mod bco;
pub mod blocker;
/// M3 — maps every compatibility/validation boundary failure to a
/// typed BlockerKind plus at least one populated UnblockPath.
pub mod boundary_actionability;
/// Builder module.
pub mod builder;
pub mod checkpoint_mode;
pub mod claim_contract;
pub mod claim_extractor;
pub mod claim_repair;
pub mod claim_sink;
pub mod claim_verifier;
/// Classify module.
pub mod classify;
pub mod classify_gates;
pub mod clock;
pub mod compatibility;
pub mod composite_score;
// v4 P8 (D1 / F23) — compile-time type discipline. The module is
// declared `pub` here so internal `crate::compile_time_discipline::*`
// paths resolve, but the submodules below are `pub(crate)` so the
// phantom-typed wrappers (`AlignedReads<R>`, `Interval<C>`,
// `PortInPass<S>`, `KMer<K>`) never cross the crate boundary.
// `lib.rs` does NOT add a `pub use...` re-export.
pub mod compile_time_discipline;
pub mod composer;
pub mod composer_v4;
// Typed Config struct loaded once at process boot via
// Config::from_env; strict parsers reject NaN/INF, non-https Anthropic
// base URLs, and out-of-range pricing multipliers.
pub mod config;
pub mod container_state;
pub mod cost;
pub mod coverage;
pub mod coverage_statement;
pub mod cross_version_diff;
// Saga: rollback for non-atomic transitions.
// ResilientClient: HTTPS-only client wrapper with scheme guards.
/// Dag module.
pub mod dag;
pub mod decision_log;
pub mod decision_substrate;
pub mod dependency_lock;
pub mod deposit_readiness;
pub mod derived_image;
pub mod resilient_client;
pub mod saga;
// Pure, deterministic descriptive-statistics helpers (DistributionStats +
// compute_distribution_stats / is_within_reference_range /
// detect_outliers_zscore). Shared by claim_verifier numeric-distribution
// checks and the harness domain-correctness assertion arms.
pub mod statistical_helpers;
// Grant v19 §Authentication of Key Resources + §Aim 3A Arm B′ —
// determinism-shim sidecar payload assembly. Emitted as
// `runtime/determinism-shim.json` by the conversation crate's
// `emit::sidecars::write_determinism_shim`.
pub mod audit_proof;
pub mod determinism_seeds;
pub mod determinism_shim;
pub mod disambiguation;
pub mod disposition;
pub mod edam;
pub mod edam_labels;
pub mod emission_invariants;
/// Emitter module.
pub mod emitter;
pub mod expected_claim;
// Shared validators (C-8 / C-9)
// for env-var names and values that flow into shell-interpolated
// commands (SSM RunCommand, sbatch --export, agent env passthrough).
// Used from crates/harness/src/executor/{aws,slurm} and from
// crates/server/src/chat_routes/remediation.rs.
pub mod env_helpers;
pub mod env_validator;
pub mod error_envelope;
pub mod expression;
pub mod external_registry;
pub mod finalize;
pub mod figure_diff;
pub mod fs_helpers;
pub mod gene_panel;
pub mod gene_panel_registry;
pub mod goal_spec;
pub mod hash_utils;
// Strongly-typed string ID newtypes (ADR 0040). TaskId starts as a
// String typedef in W4.A; flips to Arc<str> newtype in W4.B.
pub mod ids;
// Hypothesized proposal
// types (proposal lifecycle, gate outcomes) plus the
// transient/materialized TaskNode synthesizers. Consumed by the
// conversation crate's `proposal_gate` runner.
pub mod hypothesized_proposal;
pub mod ingestion_safety;
pub mod intake_facts;
pub mod intake_port_mapper;
pub mod intake_sanitize;
// v3 P8 — lifecycle adversarial cases (design §7).
// Encodes the six non-monotonic lifecycle edges + the
// session-scoped adjudication queue surfaced through
// `BlockerKind::AdjudicationRequired`.
pub mod lifecycle_adversarial;
pub mod llm_availability;
pub mod local_extension_graduation;
// v3 P7 — schema-version migration registry + WRROC replay API.
// Closes design see `migration::SchemaVersionsManifest` for
// the per-package manifest written at emit time and
// `migration::replay_provenance` for the read-side replay surface.
pub mod method_landscape;
pub mod migration;
pub mod modality_bounds;
pub mod modality_registry;
pub mod ontology_scope;
// Import-side helpers: probe an uploaded package's completeness (which
// reproducibility features it physically supports) and reconstruct a
// WorkflowDag from the on-disk crate. Pure, sync, no tokio.
pub mod package_import;
pub mod plot_affordance;
/// Deterministic zero-JS `ro-crate-preview.html` renderer. Pure function of
/// the `@graph`; no clock, no RNG, no HashMap, no host paths.
pub mod preview;
pub mod policy_context;
pub mod policy_schema;
pub mod population_coverage;
pub mod preferred_methods;
pub mod project_class;
pub mod project_class_registry;
// V4 alignment validation × lifecycle promotion
// grid loaded from `config/promotion-gate-policy.yaml`. Consulted by
// `composer_v4::policy_gate::consult_promotion_gate` at every
// promotion attempt; F19 forbids ad-hoc promotion in code.
pub mod promotion_gate_policy;
pub mod provenance_tiers;
pub mod reexecution;
pub mod reexecution_bounds;
pub mod registry;
pub mod remediation;
// v4 P5 (D5 / F20) — repair-strategy registry. Planner consults this
// module when `meet_in_the_middle` returns Disconnected /
// PartiallyConnected with gaps. Strategies produce typed
// `RepairProposal`s; only `LowAutoAttempt`-classified proposals auto-
// apply, and substrate emission is mandatory for every proposal.
pub mod repair;
/// Iterative repair loop: classifies execution failures, attempts bounded
/// per-class repairs, and routes agentic needs to review. Distinct from
/// the V4 DAG `repair` registry above.
pub mod repair_loop;
pub mod replay;
pub mod retrieval_routes;
/// Ro crate module.
pub mod ro_crate;
pub mod rubric_self_assessment;
pub mod runtime_prereqs;
pub mod sandbox_policy;
pub mod sandbox_refusal_category;
pub mod schema_helpers;
pub mod session_mode;
pub mod stage_labels;
pub mod strata;
pub mod taxonomy;
pub mod time_helpers;
pub mod validation_obligations;
pub mod workflow_contracts;
pub mod wrroc_validator;
