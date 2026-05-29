//! Provenance privacy tiers.
//!
//! Tiers per design §10:
//!
//! - `Private` — full trace; access-controlled. May contain
//!   prompt/response text, raw uploads, internal session IDs.
//! - `RedactedAudit` — no PHI / secrets / proprietary raw prompt
//!   text. Suitable for cross-team review. Decision-log
//!   timestamps are coarsened to ISO date (no time-of-day).
//! - `ExportablePublic` — RO-Crate / WRROC / PROV-O subset that
//!   may be published. Strips author identifiers, free-text
//!   rationales, and any text that hasn't been explicitly
//!   marked public-safe.
//! - `Suppressed` — v3 §10.1 fourth mode. Used when even a hash
//!   or redacted form would preserve harmful content. Carries
//!   `ChainOfCustody` so auditors can still trace provenance
//!   through `runtime/policy-decisions.jsonl`.
//!
//! This module ships the tier discriminator + redaction policy
//! types. The tier selector is wired into
//! `crates/conversation/src/emit/mod.rs::emit_with_conversation_log`
//! so a single emitter call produces the right RO-Crate slice
//! per tier. The crates/core side ships the redactor that
//! conversation calls.
//!
//! v3 P5 adds (1) the `Suppressed` tier variant with a dedicated
//! `redact_record_suppressed` helper that drops every field except
//! id + chain-of-custody + a `suppressed: true` sentinel, and (2)
//! `detect_phi_leak` — an F16 PHI-pattern scan over a JSONL stream
//! that the emit pipeline calls before persisting anything in a
//! non-Private tier. A non-empty leak set blocks the emit.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::sync::OnceLock;
use ts_rs::TS;

use crate::decision_log::{DecisionRecord, DecisionType};
use crate::ids::{StageId, TaskId};

/// Four-state provenance tier discriminator. `Suppressed` is the
/// fourth mode, used when even a hash or redacted form would preserve harmful content.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Ord, PartialOrd, TS, Default,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceTier {
    #[default]
    /// Private variant.
    Private,
    /// RedactedAudit variant.
    RedactedAudit,
    /// ExportablePublic variant.
    ExportablePublic,
    /// v3 §10.1 fourth mode — used when even a hash or redacted form
    /// would preserve harmful content. Carries `ChainOfCustody` so
    /// auditors can still trace provenance.
    Suppressed,
}

/// Redaction policy. Site-local installations can extend this to
/// cover additional PHI/secret patterns beyond the minimum set.
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
pub struct RedactionPolicy {
    /// Decision-log fields to clear when emitting at the
    /// `RedactedAudit` tier.
    pub redact_at_audit: BTreeSet<String>,
    /// Decision-log fields to clear (or fully drop entries) when
    /// emitting at the `ExportablePublic` tier.
    pub redact_at_public: BTreeSet<String>,
}

impl RedactionPolicy {
    /// Default policy: drop free-text fields at audit; drop the
    /// fields plus the rationale entirely at public.
    pub fn default_policy() -> Self {
        let mut redact_at_audit = BTreeSet::new();
        redact_at_audit.insert("body".into());
        redact_at_audit.insert("fragment".into());
        redact_at_audit.insert("method_prose".into());
        redact_at_audit.insert("rationale".into());

        let mut redact_at_public = redact_at_audit.clone();
        redact_at_public.insert("statement".into()); // assumption text
        redact_at_public.insert("author".into());
        redact_at_public.insert("reason".into());
        // Affordance-specific redactions at ExportablePublic:
        // `figure_ids` may encode stage-internal naming; drop at public.
        // `fallback_reason` may echo SME-derived semantic-type text; drop at public.
        redact_at_public.insert("figure_ids".into());
        redact_at_public.insert("fallback_reason".into());
        // Renderer generation. At audit we redact lint text
        // (may include user-data echoes from the LLM drafter) and
        // approver usernames (PII risk). Both are safe to preserve at
        // Private; both should be suppressed in cross-team review.
        redact_at_audit.insert("renderer_lints".into());
        redact_at_audit.insert("renderer_approver".into());
        // At public we also suppress the free-form rejection reason
        // (could echo SME-supplied prose) and downgrade all renderer
        // generation events to proposal_id-only.
        redact_at_public.insert("renderer_reject_reason".into());

        Self {
            redact_at_audit,
            redact_at_public,
        }
    }
}

/// Apply tier redaction to a JSONL stream of compatibility
/// proofs (one `CompatibilityProof` per line). At the `RedactedAudit`
/// tier we strip free-text rationales; at `ExportablePublic` we also
/// drop warnings (which can carry SME-supplied prose). `Private` is
/// pass-through. v3 P5 — at `Suppressed` we drop every field except
/// id / kind / chain_of_custody and mark `suppressed: true`. Returns
/// the redacted JSONL string.
pub fn redact_proofs_jsonl(jsonl: &str, tier: ProvenanceTier) -> String {
    if matches!(tier, ProvenanceTier::Private) {
        return jsonl.to_string();
    }
    let suppressed = matches!(tier, ProvenanceTier::Suppressed);
    let strip_rationale = matches!(
        tier,
        ProvenanceTier::RedactedAudit | ProvenanceTier::ExportablePublic
    );
    let strip_warnings = matches!(tier, ProvenanceTier::ExportablePublic);
    let mut out = String::new();
    for line in jsonl.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if suppressed {
            redact_record_suppressed(&mut value);
        } else if let Some(obj) = value.as_object_mut() {
            if strip_rationale && obj.contains_key("rationale") {
                obj.insert(
                    "rationale".into(),
                    serde_json::Value::String("[redacted]".into()),
                );
            }
            if strip_warnings {
                obj.remove("warnings");
            }
        }
        if let Ok(line) = serde_json::to_string(&value) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// Strip provenance assumptions to the configured emit tier (matches
/// the policy applied to proofs.jsonl by `redact_proofs_jsonl`).
pub fn redact_assumptions_jsonl(payload: String, tier: ProvenanceTier) -> String {
    redact_proofs_jsonl(&payload, tier)
}

/// v3 P5 — at the `Suppressed` tier, drop every field on the record
/// except identifiers (`id` / `kind`) and the load-bearing
/// `chain_of_custody`, and stamp `suppressed: true` so consumers can
/// detect the mode without re-reading the tier label. Idempotent: a
/// record without an `id`/`kind` keeps the empty object shape rather
/// than failing.
fn redact_record_suppressed(record: &mut serde_json::Value) {
    let id = record.get("id").cloned();
    let custody = record.get("chain_of_custody").cloned();
    let kind = record.get("kind").cloned();
    if let Some(obj) = record.as_object_mut() {
        obj.clear();
        if let Some(v) = id {
            obj.insert("id".into(), v);
        }
        if let Some(v) = kind {
            obj.insert("kind".into(), v);
        }
        if let Some(v) = custody {
            obj.insert("chain_of_custody".into(), v);
        }
        obj.insert("suppressed".into(), serde_json::Value::Bool(true));
    }
}

/// Apply tier redaction to a JSONL stream of validation
/// reports. At `RedactedAudit` we redact free-text `notes`; at
/// `ExportablePublic` we also drop per-row `details` (which can
/// contain SME-readable but non-public prose). v3 P5 — at
/// `Suppressed` we drop every field except id/kind/custody and mark
/// `suppressed: true`.
pub fn redact_validation_reports_jsonl(jsonl: &str, tier: ProvenanceTier) -> String {
    if matches!(tier, ProvenanceTier::Private) {
        return jsonl.to_string();
    }
    let suppressed = matches!(tier, ProvenanceTier::Suppressed);
    let strip_notes = matches!(
        tier,
        ProvenanceTier::RedactedAudit | ProvenanceTier::ExportablePublic
    );
    let strip_details = matches!(tier, ProvenanceTier::ExportablePublic);
    let mut out = String::new();
    for line in jsonl.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if suppressed {
            redact_record_suppressed(&mut value);
        } else if let Some(obj) = value.as_object_mut() {
            if strip_notes && obj.contains_key("notes") {
                obj.insert(
                    "notes".into(),
                    serde_json::Value::String("[redacted]".into()),
                );
            }
            if strip_details {
                obj.remove("details");
            }
        }
        if let Ok(line) = serde_json::to_string(&value) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// Apply the redaction policy to a `DecisionRecord`. Returns a
/// new record with the appropriate fields cleared. Records that
/// would lose all useful information at `ExportablePublic`
/// (e.g. `UserNote`) are dropped entirely; the caller checks
/// for `None`.
///
/// v3 P5 — at the `Suppressed` tier, the record is reshaped to
/// preserve only `session_id` + `actor` + `chain_of_custody` and
/// the decision payload is replaced by an empty `UserNote` with the
/// body `[suppressed]`. The caller (emit pipeline) is expected to
/// only invoke this path on records that genuinely carry suppressed
/// content; ordinary records remain at `Private` / `RedactedAudit`.
pub fn redact_record(
    record: &DecisionRecord,
    tier: ProvenanceTier,
    policy: &RedactionPolicy,
) -> Option<DecisionRecord> {
    if matches!(tier, ProvenanceTier::Private) {
        return Some(record.clone());
    }
    if matches!(tier, ProvenanceTier::Suppressed) {
        // v3 P5 — preserve only id-bearing fields + custody. The
        // decision payload is replaced by a `[suppressed]` `UserNote`
        // so the record still parses as a valid `DecisionRecord`.
        let mut redacted = record.clone();
        redacted.decision = DecisionType::UserNote {
            task_id: TaskId::default(),
            body: "[suppressed]".into(),
            author: String::new(),
        };
        redacted.rationale = None;
        return Some(redacted);
    }
    let drop_at_public = matches!(tier, ProvenanceTier::ExportablePublic);
    let redact_set = if drop_at_public {
        &policy.redact_at_public
    } else {
        &policy.redact_at_audit
    };

    let mut redacted = record.clone();
    if drop_at_public && matches!(redacted.decision, DecisionType::UserNote { .. }) {
        // Notes are intentionally dropped from public exports.
        return None;
    }
    if redact_set.contains("rationale") {
        redacted.rationale = redacted.rationale.map(|_| "[redacted]".into());
    }
    redacted.decision = redact_decision_type(redacted.decision, redact_set);
    Some(redacted)
}

fn redact_decision_type(d: DecisionType, redact_set: &BTreeSet<String>) -> DecisionType {
    match d {
        DecisionType::UserNote {
            task_id,
            mut body,
            mut author,
        } => {
            if redact_set.contains("body") {
                body = "[redacted]".into();
            }
            if redact_set.contains("author") {
                author = String::new();
            }
            DecisionType::UserNote {
                task_id,
                body,
                author,
            }
        }
        DecisionType::AppendIntakeProse {
            mut fragment,
            classified_modality,
            modality_changed,
        } => {
            if redact_set.contains("fragment") {
                fragment = "[redacted]".into();
            }
            DecisionType::AppendIntakeProse {
                fragment,
                classified_modality,
                modality_changed,
            }
        }
        DecisionType::AmendStage {
            stage,
            mut method_prose,
        } => {
            if redact_set.contains("method_prose") {
                method_prose = "[redacted]".into();
            }
            DecisionType::AmendStage {
                stage,
                method_prose,
            }
        }
        DecisionType::SetIntakeMethod {
            stage,
            mut method_prose,
        } => {
            if redact_set.contains("method_prose") {
                method_prose = "[redacted]".into();
            }
            DecisionType::SetIntakeMethod {
                stage,
                method_prose,
            }
        }
        DecisionType::PostHocDeviation {
            target_stage,
            prior_method,
            new_method,
            mut reason,
        } => {
            if redact_set.contains("reason") {
                reason = "[redacted]".into();
            }
            DecisionType::PostHocDeviation {
                target_stage,
                prior_method,
                new_method,
                reason,
            }
        }
        DecisionType::AssumptionRecorded {
            id,
            mut statement,
            source,
            affects_nodes,
            risk,
        } => {
            if redact_set.contains("statement") {
                statement = "[redacted]".into();
            }
            DecisionType::AssumptionRecorded {
                id,
                statement,
                source,
                affects_nodes,
                risk,
            }
        }
        // Flexible plotting upgrade plan affordance tiers.
        //
        // `PlotAffordanceResolved`:
        // - Private: full record (snapshot_id, figure_ids, provisional).
        // - RedactedAudit: passes through unchanged. The decision-log
        // entry holds `affordance_variant` + `provisional` +
        // `snapshot_id` + `figure_ids` — all PHI-free; the
        // top-level `rationale` field (on `DecisionRecord`) is
        // redacted by the caller when it contains text.
        // - ExportablePublic: drops `figure_ids` (may carry
        // stage-internal names). Variant tag + snapshot_id +
        // provisional flag remain safe to publish.
        // Driven by `"figure_ids"` in `redact_at_public`
        // (added by `default_policy()`).
        DecisionType::PlotAffordanceResolved {
            task_id,
            port_name,
            affordance_variant,
            mut figure_ids,
            provisional,
            snapshot_id,
        } => {
            // ExportablePublic: drop figure_ids (may contain
            // stage-specific internal naming). Variant tag +
            // snapshot_id + provisional flag are safe to publish.
            // `"figure_ids"` is in `redact_at_public` (added by
            // `default_policy()`) so the check is policy-
            // driven, not a hardcoded tier comparison.
            if redact_set.contains("figure_ids") {
                figure_ids = Vec::new();
            }
            DecisionType::PlotAffordanceResolved {
                task_id,
                port_name,
                affordance_variant,
                figure_ids,
                provisional,
                snapshot_id,
            }
        }
        // `PlotAffordanceFallback`:
        // - Private / RedactedAudit: full record preserved. The
        // `fallback_reason` field carries affordance-resolution
        // text (never PHI); redacting it at audit would lose
        // audit value without a privacy gain.
        // - ExportablePublic: redact `fallback_reason` (may echo
        // SME-derived semantic-type descriptions). Preserve
        // task_id + port_name + primitive + semantic_type for
        // WRROC consumers. Driven by `"fallback_reason"` in
        // `redact_at_public` (added by `default_policy()`).
        DecisionType::PlotAffordanceFallback {
            task_id,
            port_name,
            primitive,
            semantic_type,
            mut fallback_reason,
        } => {
            if redact_set.contains("fallback_reason") {
                fallback_reason = "[redacted]".into();
            }
            DecisionType::PlotAffordanceFallback {
                task_id,
                port_name,
                primitive,
                semantic_type,
                fallback_reason,
            }
        }
        // ── Flexible plotting upgrade plan sandboxed renderer
        // generation events. ────────────────────────────────────────
        //
        // `RendererDraftRequested`:
        // - Private: full record (proposal_id + model).
        // - RedactedAudit: keep proposal_id + model. No PHI risk in
        // either field.
        // - ExportablePublic: keep proposal_id only; drop model (internal
        // infrastructure detail not needed by WRROC consumers).
        DecisionType::RendererDraftRequested { proposal_id, model } => {
            let model_out = if redact_set.contains("figure_ids") {
                // `"figure_ids"` is in `redact_at_public` — use it as
                // the ExportablePublic sentinel (avoids a dedicated flag).
                String::new()
            } else {
                model
            };
            DecisionType::RendererDraftRequested {
                proposal_id,
                model: model_out,
            }
        }
        // `RendererDraftReceived`:
        // - Private: full record (proposal_id + lints).
        // - RedactedAudit: keep proposal_id; redact lints to a count
        // summary (`["<N lint messages redacted>"]`) — lint text may
        // include user-data echoes from the LLM drafter.
        // - ExportablePublic: keep proposal_id only; drop lints entirely.
        DecisionType::RendererDraftReceived { proposal_id, lints } => {
            let lints_out = if redact_set.contains("figure_ids") {
                // ExportablePublic: drop entirely.
                Vec::new()
            } else if redact_set.contains("renderer_lints") {
                // RedactedAudit: replace with count summary.
                let n = lints.len();
                vec![format!(
                    "<{n} lint message{} redacted>",
                    if n == 1 { "" } else { "s" }
                )]
            } else {
                lints
            };
            DecisionType::RendererDraftReceived {
                proposal_id,
                lints: lints_out,
            }
        }
        // `RendererSandboxOutcome`:
        // - Private: full record.
        // - RedactedAudit: keep proposal_id + outcome. The outcome is a
        // short tag (`"static_checks_passed"` / `"refused"`) — no PHI.
        // - ExportablePublic: keep proposal_id only; drop outcome.
        DecisionType::RendererSandboxOutcome {
            proposal_id,
            outcome,
        } => {
            let outcome_out = if redact_set.contains("figure_ids") {
                String::new()
            } else {
                outcome
            };
            DecisionType::RendererSandboxOutcome {
                proposal_id,
                outcome: outcome_out,
            }
        }
        // `ApproveGeneratedRenderer`:
        // - Private: full record (proposal_id + approver).
        // - RedactedAudit: keep proposal_id; replace `approver` with a
        // stable 8-hex-char SHA-256 prefix so audit trails can
        // correlate multiple approvals by the same person without
        // surfacing a PII username.
        // - ExportablePublic: keep proposal_id only; drop approver.
        DecisionType::ApproveGeneratedRenderer {
            proposal_id,
            approver,
        } => {
            let approver_out = if redact_set.contains("figure_ids") {
                // ExportablePublic: drop entirely.
                String::new()
            } else if redact_set.contains("renderer_approver") {
                // RedactedAudit: stable hash (first 8 hex chars of SHA-256).
                let mut h = Sha256::new();
                h.update(approver.as_bytes());
                let digest = h.finalize();
                format!(
                    "approver-{:02x}{:02x}{:02x}{:02x}",
                    digest[0], digest[1], digest[2], digest[3]
                )
            } else {
                approver
            };
            DecisionType::ApproveGeneratedRenderer {
                proposal_id,
                approver: approver_out,
            }
        }
        // `RejectGeneratedRenderer`:
        // - Private: full record (proposal_id + reason).
        // - RedactedAudit: keep proposal_id; truncate `reason` to the
        // first 80 chars + `"… [redacted]"` when longer. The reason
        // text can include SME-provided prose.
        // - ExportablePublic: keep proposal_id only; drop reason.
        DecisionType::RejectGeneratedRenderer {
            proposal_id,
            reason,
        } => {
            let reason_out = if redact_set.contains("renderer_reject_reason") {
                // ExportablePublic key present → drop entirely.
                String::new()
            } else if redact_set.contains("renderer_approver") {
                // RedactedAudit: truncate to first 80 chars.
                if reason.len() > 80 {
                    format!("{}… [redacted]", &reason[..80])
                } else {
                    reason
                }
            } else {
                reason
            };
            DecisionType::RejectGeneratedRenderer {
                proposal_id,
                reason: reason_out,
            }
        }
        // `PromotedGeneratedRenderer`:
        // - Private / RedactedAudit: full record preserved. All three
        // fields (proposal_id, target_stage_id, version) are opaque
        // identifiers / version strings — no PHI.
        // - ExportablePublic: keep proposal_id only; drop target_stage_id
        // and version (internal infrastructure detail; not needed by
        // WRROC consumers).
        DecisionType::PromotedGeneratedRenderer {
            proposal_id,
            target_stage_id,
            version,
        } => {
            let (stage_out, version_out) = if redact_set.contains("figure_ids") {
                // ExportablePublic: drop both fields.
                (StageId::new(""), String::new())
            } else {
                (target_stage_id, version)
            };
            DecisionType::PromotedGeneratedRenderer {
                proposal_id,
                target_stage_id: stage_out,
                version: version_out,
            }
        }
        other => other,
    }
}

// ── F16 — PHI-scope leak detection ─────────────────────────────────
//
// `detect_phi_leak` scans a JSONL stream with a fixed set of regex
// patterns (MRN, DOB, US phone, US SSN, email, GPS coords). When the
// target tier is `Private` no scan runs (Private is access-controlled
// by definition). For any other tier, a non-empty leak set is the
// signal to the emit pipeline to refuse the emit — closing the v3
// §10.6 PHI-scope leak gap.

/// One PHI leak finding produced by `detect_phi_leak`.
/// Stable record so the UI and SME-facing refusal reports can render
/// the offending line, the JSON pointer, and the matched pattern.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct PhiLeak {
    /// 1-based line number in the scanned JSONL stream.
    pub line: usize,
    /// JSON pointer (`/foo/bar`) to the offending value. Empty when
    /// the match was at the document root.
    pub field: String,
    /// Stable pattern name (`mrn`, `dob`, `phone`, `ssn`, `email`,
    /// `gps_coords`).
    pub pattern_name: String,
}

struct PhiPattern {
    name: &'static str,
    regex: regex::Regex,
}

/// Registered PHI patterns. Lazy because `regex::Regex` is not
/// `const`-constructible.
fn phi_patterns() -> &'static [PhiPattern] {
    static PATTERNS: OnceLock<Vec<PhiPattern>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            PhiPattern {
                name: "ssn",
                regex: regex::Regex::new(r"\b\d{3}-\d{2}-\d{4}\b").expect("ssn regex compiles"),
            },
            PhiPattern {
                name: "phone",
                regex: regex::Regex::new(
                    r"\b(?:\+?\d{1,3}[\s\-.]?)?(?:\(\d{3}\)|\d{3})[\s\-.]?\d{3}[\s\-.]?\d{4}\b",
                )
                .expect("phone regex compiles"),
            },
            PhiPattern {
                name: "email",
                regex: regex::Regex::new(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b")
                    .expect("email regex compiles"),
            },
            PhiPattern {
                name: "dob",
                regex: regex::Regex::new(r"\b\d{4}-\d{2}-\d{2}\b").expect("dob regex compiles"),
            },
            PhiPattern {
                name: "mrn",
                regex: regex::Regex::new(r"(?i)\bMRN\b[^\d]{0,4}\d{6,}")
                    .expect("mrn regex compiles"),
            },
            PhiPattern {
                name: "gps_coords",
                regex: regex::Regex::new(r"-?\d{1,3}\.\d{4,}\s*,\s*-?\d{1,3}\.\d{4,}")
                    .expect("gps regex compiles"),
            },
        ]
    })
}

/// JSON keys whose values are always non-PHI by design — emit-side
/// timestamps, deterministic ids, etc. Skip them to keep false-positive
/// rates manageable.
fn is_non_phi_key(key: &str) -> bool {
    matches!(
        key,
        "timestamp" | "@id" | "id" | "session_id" | "snapshot_id" | "suppression_timestamp"
    )
}

/// F16 — detect PHI content that would leak under the requested tier.
/// Returns the set of fields containing patterns matching the PHI rule
/// set; if non-empty, emit-time refuses to write.
///
/// `Private` is access-controlled by definition; the scan short-
/// circuits there. Every other tier runs the full pattern set against
/// every JSON-string-valued leaf in the JSONL stream and reports each
/// match with a JSON pointer.
pub fn detect_phi_leak(jsonl: &str, target_tier: ProvenanceTier) -> Vec<PhiLeak> {
    if matches!(target_tier, ProvenanceTier::Private) {
        return vec![];
    }
    let mut leaks = Vec::new();
    let patterns = phi_patterns();
    for (idx, raw) in jsonl.lines().enumerate() {
        let line_no = idx + 1;
        if raw.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(_) => continue,
        };
        scan_value(&value, "", patterns, line_no, &mut leaks);
    }
    leaks
}

fn scan_value(
    value: &serde_json::Value,
    pointer: &str,
    patterns: &[PhiPattern],
    line_no: usize,
    out: &mut Vec<PhiLeak>,
) {
    match value {
        serde_json::Value::String(s) => {
            for pattern in patterns {
                if pattern.regex.is_match(s) {
                    out.push(PhiLeak {
                        line: line_no,
                        field: pointer.to_string(),
                        pattern_name: pattern.name.to_string(),
                    });
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for (i, item) in arr.iter().enumerate() {
                let child = format!("{pointer}/{i}");
                scan_value(item, &child, patterns, line_no, out);
            }
        }
        serde_json::Value::Object(obj) => {
            for (k, v) in obj {
                if is_non_phi_key(k) {
                    continue;
                }
                let escaped = k.replace('~', "~0").replace('/', "~1");
                let child = format!("{pointer}/{escaped}");
                scan_value(v, &child, patterns, line_no, out);
            }
        }
        _ => {}
    }
}

/// Scrub well-known API-key patterns
/// from a text blob before persisting it into the emitted RO-Crate.
///
/// Applied on emit via `scrub_agent_trace_logs` over the package's
/// `runtime/outputs/**/agent-trace.log` files — keeps an SME-sharable
/// artifact free of literal secret tokens even if a debug-mode run
/// leaked them upstream.
///
/// Patterns covered (all match-and-replace with the literal string
/// `REDACTED`):
///
/// - Anthropic API keys: `sk-ant-api<N>-<base64-like>`
/// - HuggingFace tokens: `hf_<alnum>`
/// - GitHub PAT (classic): `ghp_<alnum>`
/// - GitHub PAT (fine-grained): `github_pat_<alnum_underscore>`
/// - AWS access-key id: `AKIA[0-9A-Z]{16}`
///
/// The regex set is compiled once via `OnceLock` so the hot path is
/// cheap; emit-time invocation is per-trace-log-byte but the
/// `Regex::replace_all` walk is linear in input size.
pub fn scrub_secrets(input: &str) -> String {
    static PATTERNS: OnceLock<Vec<regex::Regex>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        vec![
            // Anthropic API keys (current format: sk-ant-api03-...)
            regex::Regex::new(r"sk-ant-api[0-9]+-[A-Za-z0-9_\-]{20,}").unwrap(),
            // HuggingFace tokens
            regex::Regex::new(r"hf_[A-Za-z0-9]{20,}").unwrap(),
            // GitHub PAT (classic)
            regex::Regex::new(r"ghp_[A-Za-z0-9]{20,}").unwrap(),
            // GitHub PAT (fine-grained)
            regex::Regex::new(r"github_pat_[A-Za-z0-9_]{20,}").unwrap(),
            // AWS access-key id (fixed 20-char shape: AKIA + 16 upper/digit)
            regex::Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(),
        ]
    });
    let mut out = input.to_string();
    for re in patterns.iter() {
        out = re.replace_all(&out, "REDACTED").to_string();
    }
    out
}

/// Walk a package's `runtime/outputs/`
/// subtree and rewrite every `agent-trace.log` in place with
/// `scrub_secrets`. Idempotent.
///
/// Called from `emit_package` as defense in depth: if a prior agent
/// run under ECAA_AGENT_DEBUG=1 left a trace with an unredacted key
/// (e.g. on an older build that pre-dated the xtrace suppression in
/// scripts/agent-claude*.sh), this guarantees the emitted package
/// directory doesn't carry the literal key out the door.
///
/// Soft-fail: filesystem walk errors are surfaced as `Err` but the
/// caller wraps with `.context(...)`. A package with no
/// `runtime/outputs/` returns Ok early.
pub fn scrub_agent_trace_logs(package_dir: &std::path::Path) -> std::io::Result<usize> {
    let outputs_dir = package_dir.join("runtime").join("outputs");
    if !outputs_dir.is_dir() {
        return Ok(0);
    }
    let mut count = 0usize;
    let mut stack = vec![outputs_dir];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => continue, // soft-fail per entry; emit must not abort
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file()
                && path.file_name().and_then(|s| s.to_str()) == Some("agent-trace.log")
            {
                if let Ok(contents) = std::fs::read_to_string(&path) {
                    let scrubbed = scrub_secrets(&contents);
                    if scrubbed != contents {
                        // Best-effort atomic rewrite via.tmp + fsync + rename.
                        crate::fs_helpers::atomic_write_bytes_sync(&path, scrubbed.as_bytes())?;
                        count += 1;
                    }
                }
            }
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_log::DecisionActor;

    #[test]
    fn private_tier_is_pass_through() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::UserNote {
                task_id: "t".into(),
                body: "secret note".into(),
                author: "alan".into(),
            },
            DecisionActor::Sme,
            Some("with rationale".into()),
        );
        let out = redact_record(&rec, ProvenanceTier::Private, &policy).unwrap();
        assert_eq!(out, rec);
    }

    #[test]
    fn audit_tier_redacts_body_and_rationale() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::UserNote {
                task_id: "t".into(),
                body: "secret note".into(),
                author: "alan".into(),
            },
            DecisionActor::Sme,
            Some("rationale text".into()),
        );
        let out = redact_record(&rec, ProvenanceTier::RedactedAudit, &policy).unwrap();
        match out.decision {
            DecisionType::UserNote { body, author, .. } => {
                assert_eq!(body, "[redacted]");
                // Author is preserved at audit tier.
                assert_eq!(author, "alan");
            }
            _ => panic!("wrong variant"),
        }
        assert_eq!(out.rationale, Some("[redacted]".into()));
    }

    #[test]
    fn public_tier_drops_user_note_entirely() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::UserNote {
                task_id: "t".into(),
                body: "secret note".into(),
                author: "alan".into(),
            },
            DecisionActor::Sme,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::ExportablePublic, &policy);
        assert!(out.is_none());
    }

    #[test]
    fn public_tier_redacts_assumption_statement() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::AssumptionRecorded {
                id: "a_1".into(),
                statement: "Internal protocol detail".into(),
                source: "llm_inferred".into(),
                affects_nodes: vec![],
                risk: "low".into(),
            },
            DecisionActor::Llm,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::ExportablePublic, &policy).unwrap();
        match out.decision {
            DecisionType::AssumptionRecorded { statement, .. } => {
                assert_eq!(statement, "[redacted]");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn audit_tier_keeps_assumption_statement() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::AssumptionRecorded {
                id: "a_1".into(),
                statement: "GRCh38 assumed".into(),
                source: "llm_inferred".into(),
                affects_nodes: vec![],
                risk: "low".into(),
            },
            DecisionActor::Llm,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::RedactedAudit, &policy).unwrap();
        match out.decision {
            DecisionType::AssumptionRecorded { statement, .. } => {
                // Audit tier doesn't redact assumption statement
                // (the policy only redacts it at public).
                assert_eq!(statement, "GRCh38 assumed");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn tier_ordering_is_stable() {
        assert!(ProvenanceTier::Private < ProvenanceTier::RedactedAudit);
        assert!(ProvenanceTier::RedactedAudit < ProvenanceTier::ExportablePublic);
    }

    /// PlotAffordanceResolved at ExportablePublic drops figure_ids.
    #[test]
    fn plot_affordance_resolved_public_drops_figure_ids() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::PlotAffordanceResolved {
                task_id: "differential_expression".into(),
                port_name: "result_table".into(),
                affordance_variant: "registered".into(),
                figure_ids: vec!["volcano".into(), "ma_plot".into()],
                provisional: false,
                snapshot_id: "snap-2026-05-08-a".into(),
            },
            DecisionActor::Llm,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::ExportablePublic, &policy).unwrap();
        match out.decision {
            DecisionType::PlotAffordanceResolved {
                figure_ids,
                affordance_variant,
                snapshot_id,
                provisional,
                ..
            } => {
                assert!(
                    figure_ids.is_empty(),
                    "ExportablePublic must drop figure_ids"
                );
                assert_eq!(affordance_variant, "registered");
                assert_eq!(snapshot_id, "snap-2026-05-08-a");
                assert!(!provisional);
            }
            _ => panic!("wrong variant"),
        }
    }

    /// PlotAffordanceResolved at RedactedAudit preserves all fields.
    #[test]
    fn plot_affordance_resolved_audit_preserves_all_fields() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::PlotAffordanceResolved {
                task_id: "differential_expression".into(),
                port_name: "result_table".into(),
                affordance_variant: "registered".into(),
                figure_ids: vec!["volcano".into()],
                provisional: false,
                snapshot_id: "snap-x".into(),
            },
            DecisionActor::Llm,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::RedactedAudit, &policy).unwrap();
        match out.decision {
            DecisionType::PlotAffordanceResolved { figure_ids, .. } => {
                assert_eq!(figure_ids, vec!["volcano".to_string()]);
            }
            _ => panic!("wrong variant"),
        }
    }

    /// PlotAffordanceFallback at ExportablePublic redacts fallback_reason.
    #[test]
    fn plot_affordance_fallback_public_redacts_reason() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::PlotAffordanceFallback {
                task_id: "qc_preprocessing".into(),
                port_name: "qc_metrics".into(),
                primitive: "distribution".into(),
                semantic_type: "data:9999".into(),
                fallback_reason: "no registered renderer for this semantic type".into(),
            },
            DecisionActor::Llm,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::ExportablePublic, &policy).unwrap();
        match out.decision {
            DecisionType::PlotAffordanceFallback {
                fallback_reason,
                primitive,
                semantic_type,
                ..
            } => {
                assert_eq!(fallback_reason, "[redacted]");
                // Non-sensitive fields preserved.
                assert_eq!(primitive, "distribution");
                assert_eq!(semantic_type, "data:9999");
            }
            _ => panic!("wrong variant"),
        }
    }

    /// PlotAffordanceFallback at RedactedAudit preserves all fields.
    #[test]
    fn plot_affordance_fallback_audit_preserves_reason() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::PlotAffordanceFallback {
                task_id: "qc_preprocessing".into(),
                port_name: "qc_metrics".into(),
                primitive: "distribution".into(),
                semantic_type: "data:9999".into(),
                fallback_reason: "no registered renderer".into(),
            },
            DecisionActor::Llm,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::RedactedAudit, &policy).unwrap();
        match out.decision {
            DecisionType::PlotAffordanceFallback {
                fallback_reason, ..
            } => {
                assert_eq!(fallback_reason, "no registered renderer");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn append_intake_prose_redacts_fragment_at_audit() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::AppendIntakeProse {
                fragment: "patient identifier in prose".into(),
                classified_modality: "bulk_rnaseq".into(),
                modality_changed: false,
            },
            DecisionActor::Sme,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::RedactedAudit, &policy).unwrap();
        match out.decision {
            DecisionType::AppendIntakeProse {
                fragment,
                classified_modality,
                ..
            } => {
                assert_eq!(fragment, "[redacted]");
                // Classification metadata is preserved.
                assert_eq!(classified_modality, "bulk_rnaseq");
            }
            _ => panic!("wrong variant"),
        }
    }

    // ── sandboxed renderer generation redaction tests ──────────

    /// RendererDraftRequested: Private keeps all fields.
    #[test]
    fn renderer_draft_requested_private_preserves_all() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::RendererDraftRequested {
                proposal_id: "prop-abc".into(),
                model: "opus_4_8".into(),
            },
            DecisionActor::Llm,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::Private, &policy).unwrap();
        match out.decision {
            DecisionType::RendererDraftRequested { proposal_id, model } => {
                assert_eq!(proposal_id, "prop-abc");
                assert_eq!(model, "opus_4_8");
            }
            _ => panic!("wrong variant"),
        }
    }

    /// RendererDraftRequested: RedactedAudit keeps both fields
    /// (neither is PHI).
    #[test]
    fn renderer_draft_requested_audit_preserves_both_fields() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::RendererDraftRequested {
                proposal_id: "prop-abc".into(),
                model: "opus_4_8".into(),
            },
            DecisionActor::Llm,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::RedactedAudit, &policy).unwrap();
        match out.decision {
            DecisionType::RendererDraftRequested { proposal_id, model } => {
                assert_eq!(proposal_id, "prop-abc");
                assert_eq!(model, "opus_4_8");
            }
            _ => panic!("wrong variant"),
        }
    }

    /// RendererDraftRequested: ExportablePublic drops model.
    #[test]
    fn renderer_draft_requested_public_drops_model() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::RendererDraftRequested {
                proposal_id: "prop-abc".into(),
                model: "opus_4_8".into(),
            },
            DecisionActor::Llm,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::ExportablePublic, &policy).unwrap();
        match out.decision {
            DecisionType::RendererDraftRequested { proposal_id, model } => {
                assert_eq!(proposal_id, "prop-abc");
                assert!(model.is_empty(), "ExportablePublic must drop model");
            }
            _ => panic!("wrong variant"),
        }
    }

    /// RendererDraftReceived: Private keeps full lints.
    #[test]
    fn renderer_draft_received_private_preserves_lints() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::RendererDraftReceived {
                proposal_id: "prop-abc".into(),
                lints: vec!["unused import".into(), "style: prefer explicit type".into()],
            },
            DecisionActor::Llm,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::Private, &policy).unwrap();
        match out.decision {
            DecisionType::RendererDraftReceived { lints, .. } => {
                assert_eq!(lints.len(), 2);
                assert_eq!(lints[0], "unused import");
            }
            _ => panic!("wrong variant"),
        }
    }

    /// RendererDraftReceived: RedactedAudit collapses lints to count.
    #[test]
    fn renderer_draft_received_audit_redacts_lints_to_count() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::RendererDraftReceived {
                proposal_id: "prop-abc".into(),
                lints: vec!["lint one".into(), "lint two".into(), "lint three".into()],
            },
            DecisionActor::Llm,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::RedactedAudit, &policy).unwrap();
        match out.decision {
            DecisionType::RendererDraftReceived { proposal_id, lints } => {
                assert_eq!(proposal_id, "prop-abc");
                assert_eq!(lints.len(), 1, "should be collapsed to one summary entry");
                assert_eq!(lints[0], "<3 lint messages redacted>");
            }
            _ => panic!("wrong variant"),
        }
    }

    /// RendererDraftReceived: RedactedAudit singular count form.
    #[test]
    fn renderer_draft_received_audit_singular_count() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::RendererDraftReceived {
                proposal_id: "prop-abc".into(),
                lints: vec!["one lint".into()],
            },
            DecisionActor::Llm,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::RedactedAudit, &policy).unwrap();
        match out.decision {
            DecisionType::RendererDraftReceived { lints, .. } => {
                assert_eq!(lints[0], "<1 lint message redacted>");
            }
            _ => panic!("wrong variant"),
        }
    }

    /// RendererDraftReceived: ExportablePublic drops lints entirely.
    #[test]
    fn renderer_draft_received_public_drops_lints() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::RendererDraftReceived {
                proposal_id: "prop-abc".into(),
                lints: vec!["lint".into()],
            },
            DecisionActor::Llm,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::ExportablePublic, &policy).unwrap();
        match out.decision {
            DecisionType::RendererDraftReceived { lints, .. } => {
                assert!(lints.is_empty(), "ExportablePublic must drop lints");
            }
            _ => panic!("wrong variant"),
        }
    }

    /// RendererSandboxOutcome: Private / RedactedAudit keep outcome.
    #[test]
    fn renderer_sandbox_outcome_audit_keeps_outcome() {
        let policy = RedactionPolicy::default_policy();
        for tier in [ProvenanceTier::Private, ProvenanceTier::RedactedAudit] {
            let rec = DecisionRecord::new(
                "s1",
                DecisionType::RendererSandboxOutcome {
                    proposal_id: "prop-abc".into(),
                    outcome: "static_checks_passed".into(),
                },
                DecisionActor::Llm,
                None,
            );
            let out = redact_record(&rec, tier, &policy).unwrap();
            match out.decision {
                DecisionType::RendererSandboxOutcome { outcome, .. } => {
                    assert_eq!(
                        outcome, "static_checks_passed",
                        "tier {tier:?} must keep outcome"
                    );
                }
                _ => panic!("wrong variant for tier {tier:?}"),
            }
        }
    }

    /// RendererSandboxOutcome: ExportablePublic drops outcome.
    #[test]
    fn renderer_sandbox_outcome_public_drops_outcome() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::RendererSandboxOutcome {
                proposal_id: "prop-abc".into(),
                outcome: "static_checks_passed".into(),
            },
            DecisionActor::Llm,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::ExportablePublic, &policy).unwrap();
        match out.decision {
            DecisionType::RendererSandboxOutcome {
                proposal_id,
                outcome,
            } => {
                assert_eq!(proposal_id, "prop-abc");
                assert!(outcome.is_empty(), "ExportablePublic must drop outcome");
            }
            _ => panic!("wrong variant"),
        }
    }

    /// ApproveGeneratedRenderer: Private keeps real approver.
    #[test]
    fn approve_generated_renderer_private_keeps_approver() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::ApproveGeneratedRenderer {
                proposal_id: "prop-abc".into(),
                approver: "alan".into(),
            },
            DecisionActor::Sme,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::Private, &policy).unwrap();
        match out.decision {
            DecisionType::ApproveGeneratedRenderer { approver, .. } => {
                assert_eq!(approver, "alan");
            }
            _ => panic!("wrong variant"),
        }
    }

    /// ApproveGeneratedRenderer: RedactedAudit replaces with hash.
    #[test]
    fn approve_generated_renderer_audit_hashes_approver() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::ApproveGeneratedRenderer {
                proposal_id: "prop-abc".into(),
                approver: "alan".into(),
            },
            DecisionActor::Sme,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::RedactedAudit, &policy).unwrap();
        match out.decision {
            DecisionType::ApproveGeneratedRenderer {
                proposal_id,
                approver,
            } => {
                assert_eq!(proposal_id, "prop-abc");
                // Hash must start with "approver-" and be stable.
                assert!(
                    approver.starts_with("approver-"),
                    "RedactedAudit approver must be a stable hash, got: {approver}"
                );
                // 8 lowercase hex chars after the prefix.
                let hex_part = approver.trim_start_matches("approver-");
                assert_eq!(hex_part.len(), 8, "expected 8 hex chars, got: {hex_part}");
                // Idempotent: same input always produces the same hash.
                let rec2 = DecisionRecord::new(
                    "s1",
                    DecisionType::ApproveGeneratedRenderer {
                        proposal_id: "prop-abc".into(),
                        approver: "alan".into(),
                    },
                    DecisionActor::Sme,
                    None,
                );
                let out2 = redact_record(&rec2, ProvenanceTier::RedactedAudit, &policy).unwrap();
                match out2.decision {
                    DecisionType::ApproveGeneratedRenderer { approver: a2, .. } => {
                        assert_eq!(approver, a2, "hash must be stable across calls");
                    }
                    _ => panic!("wrong variant"),
                }
            }
            _ => panic!("wrong variant"),
        }
    }

    /// ApproveGeneratedRenderer: ExportablePublic drops approver.
    #[test]
    fn approve_generated_renderer_public_drops_approver() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::ApproveGeneratedRenderer {
                proposal_id: "prop-abc".into(),
                approver: "alan".into(),
            },
            DecisionActor::Sme,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::ExportablePublic, &policy).unwrap();
        match out.decision {
            DecisionType::ApproveGeneratedRenderer {
                proposal_id,
                approver,
            } => {
                assert_eq!(proposal_id, "prop-abc");
                assert!(approver.is_empty(), "ExportablePublic must drop approver");
            }
            _ => panic!("wrong variant"),
        }
    }

    /// RejectGeneratedRenderer: Private keeps full reason.
    #[test]
    fn reject_generated_renderer_private_keeps_reason() {
        let policy = RedactionPolicy::default_policy();
        let long_reason = "the output was not what I expected — full details here";
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::RejectGeneratedRenderer {
                proposal_id: "prop-abc".into(),
                reason: long_reason.into(),
            },
            DecisionActor::Sme,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::Private, &policy).unwrap();
        match out.decision {
            DecisionType::RejectGeneratedRenderer { reason, .. } => {
                assert_eq!(reason, long_reason);
            }
            _ => panic!("wrong variant"),
        }
    }

    /// RejectGeneratedRenderer: RedactedAudit keeps short reasons verbatim.
    #[test]
    fn reject_generated_renderer_audit_keeps_short_reason() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::RejectGeneratedRenderer {
                proposal_id: "prop-abc".into(),
                reason: "axes mislabelled".into(),
            },
            DecisionActor::Sme,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::RedactedAudit, &policy).unwrap();
        match out.decision {
            DecisionType::RejectGeneratedRenderer { reason, .. } => {
                assert_eq!(reason, "axes mislabelled");
            }
            _ => panic!("wrong variant"),
        }
    }

    /// RejectGeneratedRenderer: RedactedAudit truncates long reasons.
    #[test]
    fn reject_generated_renderer_audit_truncates_long_reason() {
        let policy = RedactionPolicy::default_policy();
        // Build a reason longer than 80 chars.
        let long_reason = "a".repeat(100);
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::RejectGeneratedRenderer {
                proposal_id: "prop-abc".into(),
                reason: long_reason.clone(),
            },
            DecisionActor::Sme,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::RedactedAudit, &policy).unwrap();
        match out.decision {
            DecisionType::RejectGeneratedRenderer { reason, .. } => {
                assert!(
                    reason.ends_with("… [redacted]"),
                    "long reason must end with truncation marker; got: {reason}"
                );
                // The prefix must be exactly the first 80 chars.
                assert!(
                    reason.starts_with(&long_reason[..80]),
                    "truncated reason must start with first 80 chars"
                );
            }
            _ => panic!("wrong variant"),
        }
    }

    /// RejectGeneratedRenderer: ExportablePublic drops reason.
    #[test]
    fn reject_generated_renderer_public_drops_reason() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::RejectGeneratedRenderer {
                proposal_id: "prop-abc".into(),
                reason: "confidential".into(),
            },
            DecisionActor::Sme,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::ExportablePublic, &policy).unwrap();
        match out.decision {
            DecisionType::RejectGeneratedRenderer {
                proposal_id,
                reason,
            } => {
                assert_eq!(proposal_id, "prop-abc");
                assert!(reason.is_empty(), "ExportablePublic must drop reason");
            }
            _ => panic!("wrong variant"),
        }
    }

    /// PromotedGeneratedRenderer: Private / RedactedAudit keep all fields.
    #[test]
    fn promoted_generated_renderer_audit_keeps_all_fields() {
        let policy = RedactionPolicy::default_policy();
        for tier in [ProvenanceTier::Private, ProvenanceTier::RedactedAudit] {
            let rec = DecisionRecord::new(
                "s1",
                DecisionType::PromotedGeneratedRenderer {
                    proposal_id: "prop-abc".into(),
                    target_stage_id: "differential_expression".into(),
                    version: "1.0.0".into(),
                },
                DecisionActor::Llm,
                None,
            );
            let out = redact_record(&rec, tier, &policy).unwrap();
            match out.decision {
                DecisionType::PromotedGeneratedRenderer {
                    proposal_id,
                    target_stage_id,
                    version,
                } => {
                    assert_eq!(proposal_id, "prop-abc");
                    assert_eq!(
                        target_stage_id.as_str(),
                        "differential_expression",
                        "tier {tier:?} must keep target_stage_id"
                    );
                    assert_eq!(version, "1.0.0", "tier {tier:?} must keep version");
                }
                _ => panic!("wrong variant for tier {tier:?}"),
            }
        }
    }

    /// PromotedGeneratedRenderer: ExportablePublic keeps only proposal_id.
    #[test]
    fn promoted_generated_renderer_public_keeps_only_proposal_id() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::PromotedGeneratedRenderer {
                proposal_id: "prop-abc".into(),
                target_stage_id: "differential_expression".into(),
                version: "1.0.0".into(),
            },
            DecisionActor::Llm,
            None,
        );
        let out = redact_record(&rec, ProvenanceTier::ExportablePublic, &policy).unwrap();
        match out.decision {
            DecisionType::PromotedGeneratedRenderer {
                proposal_id,
                target_stage_id,
                version,
            } => {
                assert_eq!(proposal_id, "prop-abc");
                assert!(
                    target_stage_id.as_str().is_empty(),
                    "ExportablePublic must drop target_stage_id"
                );
                assert!(version.is_empty(), "ExportablePublic must drop version");
            }
            _ => panic!("wrong variant"),
        }
    }

    // ── v3 P5 — Suppressed tier + F16 leak detector ─────────────────

    /// v3 P5 — `Suppressed` tier reshapes the decision payload to a
    /// `[suppressed]` UserNote and clears rationale; session_id +
    /// custody round-trip cleanly so the audit chain stays intact.
    #[test]
    fn suppressed_tier_reshapes_record_to_minimum() {
        let policy = RedactionPolicy::default_policy();
        let rec = DecisionRecord::new(
            "s1",
            DecisionType::UserNote {
                task_id: "t".into(),
                body: "sensitive content".into(),
                author: "alan".into(),
            },
            DecisionActor::Sme,
            Some("rationale".into()),
        );
        let out = redact_record(&rec, ProvenanceTier::Suppressed, &policy).unwrap();
        match out.decision {
            DecisionType::UserNote { body, author, .. } => {
                assert_eq!(body, "[suppressed]");
                assert!(author.is_empty());
            }
            _ => panic!("Suppressed tier must reshape to UserNote sentinel"),
        }
        assert!(out.rationale.is_none());
        assert_eq!(out.session_id, "s1");
    }

    /// v3 P5 — `redact_proofs_jsonl` at `Suppressed` drops every field
    /// except id / kind / chain_of_custody and marks `suppressed: true`.
    #[test]
    fn suppressed_tier_proofs_jsonl_keeps_only_custody_fields() {
        let jsonl = r#"{"id":"p1","kind":"compat_proof","producer_type":"data:0863","rationale":"sensitive","chain_of_custody":{"suppression_class":"phi_strict","suppressing_component":"x","suppression_timestamp":"2026-05-11T00:00:00Z","policy_rule_id":"r1","auditor_access":{"kind":"permanently_deleted","deletion_authority":"a","deletion_id":"b"}}}"#;
        let out = redact_proofs_jsonl(jsonl, ProvenanceTier::Suppressed);
        let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(v["id"], "p1");
        assert_eq!(v["kind"], "compat_proof");
        assert_eq!(v["suppressed"], serde_json::Value::Bool(true));
        assert!(v.get("producer_type").is_none());
        assert!(v.get("rationale").is_none());
        assert!(v.get("chain_of_custody").is_some());
    }

    /// v3 P5 / F16 — `Private` tier short-circuits (no scan).
    #[test]
    fn detect_phi_leak_private_short_circuits() {
        let jsonl = r#"{"id":"x","body":"123-45-6789"}"#;
        let leaks = detect_phi_leak(jsonl, ProvenanceTier::Private);
        assert!(leaks.is_empty(), "Private tier must skip the scan");
    }

    /// v3 P5 / F16 — SSN pattern triggers a leak at `RedactedAudit`.
    #[test]
    fn detect_phi_leak_ssn_in_redacted_audit_blocks() {
        let jsonl = r#"{"id":"x","body":"patient SSN 123-45-6789 attached"}"#;
        let leaks = detect_phi_leak(jsonl, ProvenanceTier::RedactedAudit);
        assert_eq!(leaks.len(), 1);
        assert_eq!(leaks[0].pattern_name, "ssn");
        assert_eq!(leaks[0].field, "/body");
        assert_eq!(leaks[0].line, 1);
    }

    /// v3 P5 / F16 — email pattern fires at `ExportablePublic`.
    #[test]
    fn detect_phi_leak_email_in_public() {
        let jsonl = r#"{"id":"x","contact":"alice@example.com"}"#;
        let leaks = detect_phi_leak(jsonl, ProvenanceTier::ExportablePublic);
        assert!(leaks.iter().any(|l| l.pattern_name == "email"));
    }

    /// v3 P5 / F16 — `timestamp` keys are skipped so the dedicated
    /// audit-log timestamp doesn't fire the DOB regex.
    #[test]
    fn detect_phi_leak_skips_emit_side_timestamp() {
        let jsonl = r#"{"id":"x","timestamp":"2026-05-11"}"#;
        let leaks = detect_phi_leak(jsonl, ProvenanceTier::RedactedAudit);
        assert!(
            leaks.is_empty(),
            "non-PHI timestamp key must not trigger DOB regex"
        );
    }

    /// v3 P5 / F16 — phone pattern fires regardless of surrounding text.
    #[test]
    fn detect_phi_leak_phone_in_redacted_audit() {
        let jsonl = r#"{"id":"x","note":"call +1-555-123-4567 with results"}"#;
        let leaks = detect_phi_leak(jsonl, ProvenanceTier::RedactedAudit);
        assert!(leaks.iter().any(|l| l.pattern_name == "phone"));
    }

    /// v3 P5 / F16 — patterns are detected inside nested arrays/objects.
    #[test]
    fn detect_phi_leak_walks_nested_structure() {
        let jsonl = r#"{"id":"x","items":[{"value":"SSN: 123-45-6789"}]}"#;
        let leaks = detect_phi_leak(jsonl, ProvenanceTier::ExportablePublic);
        assert_eq!(leaks.len(), 1);
        assert_eq!(leaks[0].field, "/items/0/value");
    }

    /// v3 P5 — `Suppressed` is the largest discriminant, so the
    /// `Ord` derivation still orders it last (preserves the
    /// "tier escalates with index" invariant the rest of the
    /// codebase assumes).
    #[test]
    fn tier_ordering_places_suppressed_last() {
        assert!(ProvenanceTier::Private < ProvenanceTier::Suppressed);
        assert!(ProvenanceTier::RedactedAudit < ProvenanceTier::Suppressed);
        assert!(ProvenanceTier::ExportablePublic < ProvenanceTier::Suppressed);
    }

    // ── `scrub_secrets` ──────────────

    #[test]
    fn scrub_secrets_redacts_anthropic_key_in_trace() {
        let trace =
            "+ [agent.sh:454] export ANTHROPIC_API_KEY=sk-ant-api03-abcdef123456789012345\n";
        let scrubbed = scrub_secrets(trace);
        assert!(!scrubbed.contains("sk-ant-api03-"));
        assert!(scrubbed.contains("REDACTED"));
        // The surrounding context survives (so the trace remains useful).
        assert!(scrubbed.contains("ANTHROPIC_API_KEY="));
    }

    #[test]
    fn scrub_secrets_redacts_hf_token() {
        let trace = "HF_TOKEN=hf_aBcDeF12345678901234XYZ\n";
        let scrubbed = scrub_secrets(trace);
        assert!(!scrubbed.contains("hf_aBcDeF12345678901234XYZ"));
        assert!(scrubbed.contains("REDACTED"));
    }

    #[test]
    fn scrub_secrets_redacts_github_pat_classic() {
        let trace = "GITHUB_PERSONAL_ACCESS_TOKEN=ghp_abcdef1234567890abcdef\n";
        let scrubbed = scrub_secrets(trace);
        assert!(!scrubbed.contains("ghp_abcdef1234567890abcdef"));
        assert!(scrubbed.contains("REDACTED"));
    }

    #[test]
    fn scrub_secrets_redacts_github_pat_fine_grained() {
        let trace = "GITHUB_TOKEN=github_pat_11ABCDE0XYZ_abcdefghijklmnopqrst\n";
        let scrubbed = scrub_secrets(trace);
        assert!(!scrubbed.contains("github_pat_11ABCDE0XYZ_abcdefghijklmnopqrst"));
        assert!(scrubbed.contains("REDACTED"));
    }

    #[test]
    fn scrub_secrets_redacts_aws_access_key_id() {
        let trace = "AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE\n";
        let scrubbed = scrub_secrets(trace);
        assert!(!scrubbed.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(scrubbed.contains("REDACTED"));
    }

    #[test]
    fn scrub_secrets_handles_multiple_secrets_in_one_blob() {
        let trace = "key1=sk-ant-api03-abcdefghij1234567890abc\n\
                     key2=hf_zyxwvuts1234567890ABCDEFGH\n\
                     key3=ghp_abcdef1234567890abcdefAA\n\
                     key4=AKIAABCDEF1234567890\n";
        let scrubbed = scrub_secrets(trace);
        assert!(!scrubbed.contains("sk-ant-api03"));
        assert!(!scrubbed.contains("hf_zyxwvuts"));
        assert!(!scrubbed.contains("ghp_abcdef"));
        assert!(!scrubbed.contains("AKIAABCDEF"));
        // Should have replaced each match with REDACTED — count occurrences.
        let redacted_count = scrubbed.matches("REDACTED").count();
        assert_eq!(redacted_count, 4, "expected one REDACTED per secret");
    }

    #[test]
    fn scrub_secrets_is_idempotent() {
        let trace = "export ANTHROPIC_API_KEY=sk-ant-api03-FOOBAR12345678901234567890\n";
        let first = scrub_secrets(trace);
        let second = scrub_secrets(&first);
        assert_eq!(first, second);
    }

    #[test]
    fn scrub_secrets_no_match_returns_input_unchanged() {
        let benign = "task=integrate_layers status=running runtime=42s\n";
        let out = scrub_secrets(benign);
        assert_eq!(out, benign);
    }

    // C22 / R-7: the three `scrub_agent_trace_logs_*` integration
    // tests live at `crates/core/tests/provenance_tiers.rs` (an
    // integration-test crate where fs I/O is permissible) so this
    // production module's inline test block stays
    // pure-string-redaction only.
}
