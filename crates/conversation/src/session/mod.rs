//! Session module
//!
//! This module was the 1168-LOC `session.rs` god-module. It now wires
//! together six focused submodules:
//!
//! - [`state`] — `Session`, `SessionState`, `Turn` / `TurnRole` /
//!   `AssistantIntent`, `ConfirmationCard`, `ToolCallRecord`,
//!   `HarnessEvent`, `RemoteExecutionInfo`, `StructuredCapture*`.
//! - [`transitions`] — `try_transition()`, `StateTrigger`,
//!   `TransitionError`.
//! - [`lineage`] — `SessionLineage` + `Session::branch_from`.
//! - [`decision_helpers`] — `Session::record_decision`.
//! - [`blocker_shim`] — `SessionState::resolved_blocker` legacy bridge.
//! - [`tests`] — unit tests.
//!
//! The `Session` struct itself stays in `state.rs` (the plan keeps it
//! with the other data types). `Session::new` lives here because it's
//! the primary constructor; `impl Session` picks up the additional
//! methods from the submodules transparently (Rust allows multiple
//! `impl Session` blocks across files in the same crate).

use chrono::Utc;
use ecaa_workflow_core::builder::IntakeMethods;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for a chat session (UUID v4).
pub type SessionId = Uuid;

pub mod blocker_shim;
/// Per-emit confirmation token (replaces `user_confirmed: bool`).
pub mod confirmation_token;
pub mod cross_session_aggregator;
pub mod decision_helpers;
pub mod derived_dag;
/// Per-button single-use execution token (gates `start_execution`).
pub mod execution_token;
pub mod invalidation_guard;
pub mod lineage;
pub mod opaque_aggregator;
pub mod state;
pub mod transitions;

#[cfg(test)]
mod tests;

// Re-export the public surface so callers (server, service, tests)
// keep using `crate::session::Session`, `SessionState`, etc. without
// caring which submodule each item lives in.
pub use confirmation_token::ConfirmationToken;
pub use execution_token::ExecutionToken;
pub use invalidation_guard::WorkflowDagMut;
pub use lineage::{session_lineage_schema_version, SessionLineage};
pub use state::{
    AssistantIntent, ConfirmationCard, HarnessEvent, PendingAmendment, RemoteExecutionInfo,
    RendererProposal, RendererProposals, RetainedOptionalStage, Session, SessionState, ShareToken,
    SmeMethodSignals, StructuredCaptureField, StructuredCaptureFieldKind, StructuredCaptureTurnCard,
    ToolCallRecord, Turn, TurnRole,
};
pub use transitions::{StateTrigger, TransitionError};

/// Wrapper to make `IntakeMethods` (BTreeMap-based) serializable as JSON.
/// `IntakeResolution` doesn't derive Serialize/Deserialize in core today,
/// so we mirror its shape here.
/// Serializable wrapper around the `IntakeMethods` map.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IntakeMethodsSerde(pub std::collections::BTreeMap<String, IntakeResolutionSerde>);

/// Serializable version of `IntakeResolution` for session persistence.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IntakeResolutionSerde {
    /// The method name chosen by the SME or auto-selected.
    pub method: String,
    /// Typed key→value parameters for the method (e.g. aligner flags).
    #[serde(default)]
    pub fields: std::collections::BTreeMap<String, serde_json::Value>,
    /// Methods the SME has explicitly ruled out for this stage. The
    /// agent's best-practice scoring must filter these out BEFORE
    /// composite scoring so auto-approve can't pick them. Observed
    /// regression: SME said "NOT scVI and NOT Harmony" in intake
    /// prose; LLM captured the positive method (Seurat v5 CCA) via
    /// set_intake_method but the negative exclusions lived only in
    /// free-text prose, so auto-approve happily picked Harmony.
    #[serde(default)]
    pub excluded: Vec<String>,
}

impl IntakeMethodsSerde {
    /// Convert to the core `IntakeMethods` map.
    pub fn to_core(&self) -> IntakeMethods {
        let mut out = IntakeMethods::new();
        for (k, v) in &self.0 {
            let mut res = ecaa_workflow_core::builder::IntakeResolution::new(v.method.clone());
            for (fk, fv) in &v.fields {
                res = res.with_field(fk.clone(), fv.clone());
            }
            // Surface the exclusion list as a structured field so
            // the renderer + agent can see it. `excluded_methods`
            // (plural) is the public name we document in
            // prompt_role.txt and agent-claude.sh.
            if !v.excluded.is_empty() {
                res = res.with_field(
                    "excluded_methods".to_string(),
                    serde_json::Value::Array(
                        v.excluded
                            .iter()
                            .map(|s| serde_json::Value::String(s.clone()))
                            .collect(),
                    ),
                );
            }
            out.insert(k.clone(), res);
        }
        out
    }

    /// Set or update a method or field for a given stage.
    pub fn set(
        &mut self,
        stage: &str,
        method: Option<String>,
        field: Option<(String, serde_json::Value)>,
    ) {
        let entry = self.0.entry(stage.to_string()).or_default();
        if let Some(m) = method {
            entry.method = m;
        }
        if let Some((k, v)) = field {
            entry.fields.insert(k, v);
        }
    }

    /// Append a method to the stage's exclusion list. De-dupes
    /// case-insensitively so "Harmony" and "harmony" collapse.
    pub fn exclude(&mut self, stage: &str, method: &str) {
        let entry = self.0.entry(stage.to_string()).or_default();
        if !entry
            .excluded
            .iter()
            .any(|m| m.eq_ignore_ascii_case(method))
        {
            entry.excluded.push(method.to_string());
        }
    }
}

impl Session {
    /// Primary constructor. Additional `impl Session` blocks in
    /// `lineage.rs`, `decision_helpers.rs`, and `transitions.rs` pick
    /// up `branch_from`, `record_decision`, and `try_transition`
    /// respectively.
    pub fn new(careful_mode: bool) -> Self {
        let now = Utc::now();
        Self {
            // `schema_version` is the migration rail for the session
            // schema. Today's value is the current SemVer pin; the serde
            // adapter on the field accepts legacy `u64` reads.
            schema_version: ecaa_workflow_core::migration::current_session_version(),
            // Pilot recommendation defaults to None.
            // Set by the server's /progress handler when the harness
            // reports `sizing_pilot_complete`.
            pilot_recommendation: None,
            id: Uuid::new_v4(),
            created_at: now,
            last_activity: now,
            state: SessionState::Greeting,
            conversation: std::sync::Arc::new(vec![]),
            intake_prose: String::new(),
            intake_methods: IntakeMethodsSerde::default(),
            excluded_atoms: Vec::new(),
            // Empty SME-method-signal map for fresh sessions;
            // `set_intake_method` refuses until the UI sets a stage's
            // flag via the sme-named endpoint.
            sme_method_signals: crate::session::state::SmeMethodSignals::default(),
            classification: None,
            taxonomy: None,
            workflow_intent: None,
            project_class: Default::default(),
            mode: Default::default(),
            mode_locked: false,
            checkpoint_mode: Default::default(),
            dag: None,
            task_states: std::collections::BTreeMap::new(),
            // Fresh sessions start with no confirmation token and no
            // pending emission. The token is minted by
            // `ConversationService::confirm_with_modes` against a
            // pending_emission_id set by `propose_summary_confirmation`.
            confirmation_token: None,
            // Fresh sessions hold no execution token; the single-use
            // latch is minted only by the REST /start-execution button
            // and consumed at start_execution dispatch.
            execution_token: None,
            pending_emission_id: None,
            emitted_package_path: None,
            harness_events: vec![],
            tool_call_log: vec![],
            decisions: vec![],
            careful_mode,
            blocked_opus_escalation_consumed: false,
            lineage: None,
            title: None,
            budget_usd: read_default_budget_usd(),
            budget_set_by: read_default_budget_usd().map(|_| "env-default".to_string()),
            budget_set_at: read_default_budget_usd().map(|_| Utc::now()),
            share_tokens: vec![],
            inputs: vec![],
            pending_input_hints: vec![],
            // Default to the `local` sentinel so single-user
            // dev (loopback bind, no fronting proxy injecting
            // `X-Scripps-User`) lets every browser request through the
            // owner-authz middleware. The previous default derived
            // `owner_user` from `$USER` ("a", whatever) which then
            // required the browser to send a header it doesn't know
            // about — every per-session UI fetch hit 403. The proxy
            // path (multi-user deployment) overrides this default at
            // session-create time via `apply_owner_user` when the
            // request carried `X-Scripps-User`. See
            // crates/server/src/auth/verify_owner.rs::LOCAL_OWNER_SENTINEL.
            owner_user: "local".to_string(),
            // Fresh sessions have no pending amendment.
            // Set by `amend_stage_method` / `select_sensitivity_winner`
            // and cleared by the emit wrapper after a successful emit.
            pending_amendment: None,
            // Handler-stashed post-handler triggers; the
            // dispatcher drains this Vec after each tool call.
            deferred_state_triggers: Vec::new(),
            // Archetype snapshot is set by the composer
            // when the archetype fast-path matches. Fresh sessions
            // start with `None`; the composer pins the snapshot the
            // first time it matches an archetype for this session.
            archetype_snapshot: None,
            // V4 cache fields default to None / empty.
            // `tools::rebuild_dag` populates them on every successful
            // v4 composition.
            workflow_dag: None,
            compose_outcome: None,
            ranked_alternatives: Vec::new(),
            policy_decisions: Vec::new(),
            // No active policy bundle by default. SME
            // activates via the policy-bundle endpoint
            // (typically through ClinicalConfirmGate).
            active_policy_bundle: None,
            // Flexible plotting upgrade plan renderer proposals
            // start empty for every new session; `propose_hypothesized_renderer`
            // appends to this registry. `#[serde(default)]` on the field
            // means existing on-disk sessions without it also start empty.
            renderer_proposals: crate::session::state::RendererProposals::default(),
            // hypothesized-node proposals start empty for every new
            // session; `propose_hypothesized_node` inserts and the
            // `proposal_gate` runner advances. `#[serde(default,
            // skip_serializing_if = "BTreeMap::is_empty")]` on the
            // field means existing on-disk sessions without it also
            // start empty.
            proposals: std::collections::BTreeMap::new(),
            // Flexible plotting upgrade plan fallback counter starts
            // empty for every new session; the affordance resolver records
            // events as they occur. `#[serde(skip)]` on the field means it
            // is never persisted and always resets to empty on load/restart.
            affordance_fallback_counter:
                ecaa_workflow_core::plot_affordance::AffordanceFallbackCounter::default(),
            // v3 P8 — adjudication queue starts empty.
            adjudication_queue: Vec::new(),
            // Atom-safety-policy no runtime-package
            // overrides on a fresh session. The SME widens this via
            // the `ProvisioningDenied` BlockerCard affordance.
            atom_runtime_overrides: std::collections::BTreeMap::new(),
            // starts at 0 on every fresh session; bumped by
            // `note_turn_end_intake_followup` on each per-turn end.
            intake_followup_streak: 0,
            // Per-session 32-byte secret for HMAC audit sidecars (C5).
            // Generated once at creation with OsRng; never rotated within
            // the session's lifetime. Persisted as hex in the session JSON.
            audit_writer_secret: {
                let mut secret = [0u8; 32];
                rand::rngs::OsRng.fill_bytes(&mut secret);
                secret
            },
            // Set on first successful emit; None until then.
            last_emitted_run_id: None,
            // No outstanding disambiguation question. Set by
            // `append_intake_prose` when the classifier hits a
            // calibrated tie window; cleared by
            // `clear_disambiguation_on_selection` when the SME picks
            // a quick-reply chip.
            pending_disambiguation: None,
            // Populated by the compose path; None until a v4 composition runs.
            coverage_confidence: None,
            // No dataset probed yet on a fresh session; set by the
            // `probe_dataset` dispatch when it observes a processed-only
            // deposit (deposited product present, no raw SRA reads).
            probed_processed_only: false,
        }
    }
}

// ── Grant v19 §Authentication of Key Resources — D3 accessors ──────
//
// `runtime/security-policy.json` aggregates the per-atom SafetyPolicy
// 5-tuple across every atom the package uses, plus container image
// digests. Both accessors walk `self.workflow_dag.nodes` and resolve
// each node id against the supplied registry — the same walk the
// per-atom runtime-prereqs block in `emit::mod` performs. Container
// digests are emit-time CONTENT-HASHES (offline, deterministic,
// byte-reproducible); runtime-resolved registry digests are a separate
// tier captured in per-task evidence.

impl Session {
    /// Resolve every atom this session's DAG composes from, by walking
    /// `self.workflow_dag.nodes` and looking each node id up in the
    /// supplied registry. Mirrors the identical walk in
    /// `emit::mod`'s per-atom-runtime-prereqs block. Returns owned
    /// clones so the caller can build `&[&AtomDefinition]` for the
    /// aggregator.
    ///
    /// Always non-fatal: a missing `workflow_dag` (legacy/v1 emit) or a
    /// node id absent from the registry yields fewer atoms, never a
    /// panic — preserving the "always emits" contract.
    pub fn atoms_in_use(
        &self,
        registry: &ecaa_workflow_core::atom_registry::AtomRegistry,
    ) -> Vec<ecaa_workflow_core::atom::AtomDefinition> {
        let Some(dag) = self.workflow_dag.as_ref() else {
            return Vec::new();
        };
        dag.nodes
            .iter()
            .filter_map(|node| registry.get(node.id.as_str()).cloned())
            .collect()
    }

    /// Content-hash digests of every container this session's atoms
    /// resolve into, deterministically and OFFLINE. Two tiers:
    ///
    /// * atoms declaring a pinned `preferred_container.digest` → the
    ///   literal `@sha256:<hex>` already on the spec.
    /// * all other atoms → `content-hash:sha256:<16hex>` from
    ///   `derived_image::per_atom_image_hash` (code-only, no pull).
    ///
    /// Collected into a `BTreeSet<String>` so the output is deduped and
    /// byte-stable. Runtime-resolved registry digests are a SEPARATE
    /// tier recorded in per-task evidence (D7), never here.
    pub fn container_image_digests(
        &self,
        atoms: &[ecaa_workflow_core::atom::AtomDefinition],
    ) -> Vec<String> {
        let mut out: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for atom in atoms {
            match atom.preferred_container.as_ref() {
                Some(spec) if !spec.digest.is_empty() => {
                    out.insert(format!(
                        "@sha256:{}",
                        spec.digest.trim_start_matches("sha256:")
                    ));
                }
                _ => {
                    out.insert(format!(
                        "content-hash:sha256:{}",
                        ecaa_workflow_core::derived_image::per_atom_image_hash(atom)
                    ));
                }
            }
        }
        out.into_iter().collect()
    }

    // ── ConfirmationToken latch helpers ───────────────────────────
    //
    // Callers read `session.is_confirmed()` and mutate via
    // `mint_confirmation_token` / `clear_confirmation`.

    /// True iff the SME has confirmed THIS specific pending emission
    /// AND the plan summary hasn't drifted since the click. Three
    /// independent gates: (a) a token is present, (b) a pending
    /// emission is set, (c) the token's `(emission_id, summary_hash)`
    /// pair matches the current pending emission + summary hash.
    ///
    /// Already-emitted sessions are a special case: the durable
    /// RO-Crate on disk IS the artifact of a prior SME confirmation,
    /// so `is_confirmed()` returns true whenever `state == Emitted`
    /// and `emitted_package_path` points at a real package — even if
    /// the in-memory `confirmation_token` was cleared (the C2 legacy
    /// migration adapter folds pre-token sessions into
    /// `confirmation_token: None`, and a server restart through that
    /// adapter would otherwise make the LLM see `user_confirmed=false`
    /// on a session that has already emitted and prompt the SME to
    /// re-click Confirm). Subsequent mutations route through
    /// `amend_stage_method` or `branch_session`, both of which
    /// transition out of Emitted and explicitly clear the latch + the
    /// pending emission id, so the next emit cycle still requires a
    /// fresh click. `emit_package` itself only fires from
    /// `ReadyToEmit` → `Emitting`, so the Emitted short-circuit cannot
    /// authorize an unintended re-emit.
    ///
    /// `emit_package` uses this; the LLM prompt formatter uses this.
    pub fn is_confirmed(&self) -> bool {
        if matches!(self.state, crate::session::SessionState::Emitted)
            && self.emitted_package_path.is_some()
        {
            return true;
        }
        match (&self.confirmation_token, self.pending_emission_id) {
            (Some(token), Some(pending)) => {
                let summary = self.current_summary_hash();
                token.authorizes(pending, summary.as_str())
            }
            _ => false,
        }
    }

    /// Mint a fresh confirmation token bound to the session's current
    /// `pending_emission_id` + current summary hash. Returns `None`
    /// when there's no pending emission to bind to (caller should
    /// surface this as a `PreconditionFailure` so the SME re-enters
    /// the confirmation flow).
    ///
    /// Called by `ConversationService::confirm_with_modes` only.
    pub fn mint_confirmation_token(
        &mut self,
        granted_at: chrono::DateTime<chrono::Utc>,
        granted_by: crate::audit_actor::AuditActor,
    ) -> Option<&crate::session::ConfirmationToken> {
        let pending = self.pending_emission_id?;
        let summary = self.current_summary_hash();
        // Every confirmation-token mint is a high-impact state mutation
        // gating `emit_package`; structured-log on a dedicated target so
        // dashboards can alert on anomalous rates / sources without
        // parsing free-form messages.
        tracing::info!(
            target: "ecaa::confirmation",
            session_id = %self.id,
            operation = "set",
            emission_id = %pending,
            summary_hash = %summary,
            granted_by = ?granted_by,
            "confirmation_token mutated"
        );
        self.confirmation_token = Some(crate::session::ConfirmationToken::new(
            pending, summary, granted_at, granted_by,
        ));
        self.confirmation_token.as_ref()
    }

    /// Clear the latch. Called by every state transition that changes
    /// the emit shape (amend, branch_from, sensitivity-winner, certain
    /// Blocked variants) so the SME is forced to re-confirm the new plan.
    pub fn clear_confirmation(&mut self) {
        // Only emit when transitioning from "has token" → "no token";
        // a redundant clear (e.g. defensive call when none was set)
        // shouldn't pollute the audit-log channel.
        if self.confirmation_token.is_some() {
            tracing::info!(
                target: "ecaa::confirmation",
                session_id = %self.id,
                operation = "clear",
                emission_id = ?self.pending_emission_id,
                "confirmation_token mutated"
            );
        }
        self.confirmation_token = None;
    }

    // ── ExecutionToken latch helpers ──────────────────────────────
    //
    // Mirrors the ConfirmationToken helpers above. The token is minted
    // only by the REST `/start-execution` button (the human press) and
    // verified + consumed at `start_execution` tool dispatch so the LLM
    // cannot launch a harness run autonomously.

    /// Mint a single-use execution token bound to the current pending
    /// emission. Returns `None` when there's nothing to bind to (no
    /// pending emission); caller treats that as "no token minted".
    pub fn mint_execution_token(
        &mut self,
        granted_at: chrono::DateTime<chrono::Utc>,
        granted_by: crate::audit_actor::AuditActor,
    ) -> Option<&crate::session::ExecutionToken> {
        let request = self.pending_emission_id?;
        tracing::info!(
            target: "ecaa::execution",
            session_id = %self.id,
            operation = "set",
            request_id = %request,
            granted_by = ?granted_by,
            "execution_token mutated"
        );
        self.execution_token = Some(crate::session::ExecutionToken::new(
            request, granted_at, granted_by,
        ));
        self.execution_token.as_ref()
    }

    /// True iff a human pressed Start for THIS emission and the token
    /// hasn't been consumed. Mirrors [`Session::is_confirmed`].
    pub fn execution_authorized(&self) -> bool {
        match (&self.execution_token, self.pending_emission_id) {
            (Some(t), Some(req)) => t.authorizes(req),
            _ => false,
        }
    }

    /// Consume the execution token after a successful dispatch so a
    /// replayed `start_execution` requires a fresh Start press.
    pub fn consume_execution_token(&mut self) {
        if let Some(t) = self.execution_token.as_mut() {
            t.consume();
        }
    }

    /// Clear the latch on any emit-shape transition (amend, branch,
    /// reject). Called beside `clear_confirmation`.
    pub fn clear_execution_token(&mut self) {
        if self.execution_token.is_some() {
            tracing::info!(
                target: "ecaa::execution",
                session_id = %self.id,
                operation = "clear",
                request_id = ?self.pending_emission_id,
                "execution_token mutated"
            );
        }
        self.execution_token = None;
    }

    /// Lowercase hex SHA-256 of the CURRENT plan-shape canonical
    /// summary. `ConfirmationToken::summary_hash` is bound to this
    /// value at mint time; an amendment or re-classification will
    /// shift this digest so the token's `authorizes()` returns false
    /// even before a state transition fires `clear_confirmation`.
    ///
    /// Matches the hex shape used by
    /// [`crate::session::ConfirmationCard::summary_hash`] so an audit
    /// replayer can cross-reference the token to the card on the
    /// conversation tail byte-for-byte.
    ///
    /// Canonical input fields: `intake_methods` (BTreeMap-sorted by
    /// key), `classification.modality` (if set).
    /// These are the load-bearing "shape" of the next emit; any of
    /// them changing means the next package is materially different
    /// from what the SME approved.
    pub fn current_summary_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        let canonical = self.canonical_summary_input();
        // serde_json's BTreeMap-on-Value ordering keeps the byte
        // sequence deterministic across runs; an unstable serializer
        // would silently produce different hashes for identical
        // sessions, which would force a spurious re-confirm.
        if let Ok(s) = serde_json::to_string(&canonical) {
            h.update(s.as_bytes());
        }
        let digest = h.finalize();
        let mut hex = String::with_capacity(64);
        for byte in digest.iter() {
            use std::fmt::Write;
            let _ = write!(hex, "{byte:02x}");
        }
        hex
    }

    fn canonical_summary_input(&self) -> serde_json::Value {
        // Pick stable fields that define the "shape" of the next
        // emit. `intake_methods` is the per-stage method registry;
        // `modality` is the classifier output. Adding fields here
        // forces a re-confirm on legacy sessions where the field
        // would have been absent; we keep the set deliberately small.
        let intake = serde_json::to_value(&self.intake_methods).unwrap_or(serde_json::Value::Null);
        let modality = self
            .classification
            .as_ref()
            .map(|c| c.modality.clone())
            .unwrap_or_default();
        serde_json::json!({
            "intake_methods": intake,
            "modality": modality,
        })
    }
}

// Test fixtures. Marked `#[doc(hidden)]` so they don't show up in
// public API docs but are reachable from integration tests under
// `crates/conversation/tests/` (where `#[cfg(test)]` items in the lib
// are NOT visible — integration tests link against the regular lib).
impl Session {
    /// Test fixture — minimal session with no DAG / classifier output.
    /// Used by `crates/conversation/tests/sidecar_emission.rs` for the
    /// determinism-shim and model-policy sidecars (both of which only
    /// need a `Session::new(false)` shape).
    #[doc(hidden)]
    pub fn test_fixture_minimal() -> Self {
        Self::new(false)
    }

    /// Test fixture — session shaped to drive a DAG build. The caller
    /// is expected to run `AppendIntakeProse` via the tools dispatcher
    /// afterwards so the classifier populates the DAG — the
    /// `emit_package` core path aborts without a DAG.
    #[doc(hidden)]
    pub fn test_fixture_with_dag() -> Self {
        let mut s = Self::new(false);
        s.intake_prose =
            "single cell scRNA-seq from human IVD samples comparing degenerated and healthy".into();
        s
    }

    /// Test fixture — session shaped like one that would carry
    /// verifiable claims (today: same as `test_fixture_with_dag` —
    /// claim verification is computed per-task at runtime, not
    /// captured on the session struct).
    #[doc(hidden)]
    pub fn test_fixture_with_verifiable_claims() -> Self {
        Self::test_fixture_with_dag()
    }

    /// Test fixture — session carrying a minimal v4 `WorkflowDag` with
    /// two nodes whose ids are real stage-atom ids (`alignment`,
    /// `quantification`), so `atoms_in_use` and
    /// `container_image_digests` have content to resolve.
    #[doc(hidden)]
    pub fn test_fixture_two_atom_dag() -> Self {
        use ecaa_workflow_core::workflow_contracts::task_node::{TaskNode, WorkflowDag};
        let mut s = Self::new(false);
        let dag = WorkflowDag {
            nodes: vec![
                TaskNode::skeleton("alignment", "align"),
                TaskNode::skeleton("quantification", "quantify"),
            ],
            ..WorkflowDag::default()
        };
        s.workflow_dag = Some(dag);
        s
    }
}

/// Read `ECAA_DEFAULT_SESSION_BUDGET_USD` at construction time. Unset or
/// parse error = no default budget. Positive float = seed the session
/// with that cap. Called from `Session::new`.
fn read_default_budget_usd() -> Option<f64> {
    std::env::var("ECAA_DEFAULT_SESSION_BUDGET_USD")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
}
