//! Chain-of-custody fields composed onto every provenance
//! record. Closes the §10.2 row in `docs/dag_design_v3.md` and is the
//! enabling substrate for `ProvenanceTier::Suppressed` (the v3 §10.1
//! fourth mode) and the F16 PHI-leak detector.
//!
//! Every chain-of-custody-bearing record (edge proof, assumption,
//! decision-log row, validation report, policy decision) gets a
//! `chain_of_custody: Option<ChainOfCustody>` field. Older records
//! without the field deserialize cleanly via `#[serde(default)]`.
//!
//! Per v3 §10.2 the six load-bearing fields are: `suppression_class`,
//! `suppressing_component`, `suppression_timestamp`, `policy_rule_id`,
//! `cryptographic_commitment`, and `auditor_access`. The first four
//! are mandatory; the commitment is optional (`None` when the payload
//! is structurally guessable from a hash so the commitment is filed
//! under the restricted-archive procedure instead). The auditor
//! procedure is a tagged enum so consumers branch on retrieval kind
//! rather than free text.

use crate::workflow_contracts::policy_rule_id::PolicyRuleId;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Required chain-of-custody fields on every provenance
/// record. Composed as `Option<ChainOfCustody>` on record types so
/// older records without custody info remain valid.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ChainOfCustody {
    /// Class of suppression / redaction applied (matches a registered
    /// taxonomy ID, e.g. `phi_strict`, `prompt_injection`,
    /// `controlled_access_genotype`).
    pub suppression_class: SuppressionClass,
    /// Subsystem identifier (e.g. `provenance_tiers::redact_record`,
    /// `ingestion_safety::quarantine`).
    pub suppressing_component: String,
    /// RFC3339 timestamp.
    pub suppression_timestamp: String,
    /// Foreign key into the `policy_rules:` registry in
    /// `config/assumption-policy.yaml`. Constructed via
    /// [`PolicyRuleId::new`] in registry-aware contexts, or via
    /// [`PolicyRuleId::unchecked`] (the `From<&str>` / `From<String>`
    /// blanket impls) where the registry isn't reachable.
    pub policy_rule_id: PolicyRuleId,
    /// Cryptographic commitment (BLAKE3 hash) over the suppressed
    /// payload. `None` when the payload is structurally guessable from
    /// a hash and a non-public commitment is filed under the
    /// restricted-archive procedure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cryptographic_commitment: Option<ContentCommitment>,
    /// How an authorized auditor retrieves full content.
    pub auditor_access: AuditorProcedure,
}

/// Closed taxonomy of suppression kinds. Maps to the §15.2 policy
/// table and to ingestion-safety quarantine reasons.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum SuppressionClass {
    /// PHI strict — patient identifiers, dates of birth, etc.
    PhiStrict,
    /// Controlled-access genotype (dbGaP-style restrictions).
    ControlledAccessGenotype,
    /// Prompt-injection payload quarantined by `ingestion_safety`.
    PromptInjection,
    /// Structurally guessable identifier (e.g. short numeric MRN) for
    /// which a plain hash would still leak information; the commitment
    /// is filed restricted instead of inlined.
    StructurallyGuessableIdentifier,
    /// Raw credential captured incidentally (API key, password).
    RawCredential,
    /// User-supplied free text whose redaction status is unknown.
    UserSuppliedFreeText,
    /// Proprietary pipeline detail an operator opted out of publishing.
    ProprietaryPipelineDetail,
}

/// BLAKE3 commitment over the suppressed payload. The algorithm name
/// is preserved verbatim in the record so future curators can add
/// alternate algorithms without ambiguity at read time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ContentCommitment {
    /// Algorithm name (always `"blake3"` for records produced by
    /// `ChainOfCustody::with_commitment`).
    pub algorithm: String,
    /// Hex-encoded digest. BLAKE3 produces 32 bytes → 64 hex chars.
    pub digest_hex: String,
}

/// How an authorized auditor can retrieve the suppressed content.
/// Tagged so the UI branches on retrieval kind rather than free text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuditorProcedure {
    /// File a ticket; the restricted-archive operator retrieves
    /// content. `ticket_template` is a stable identifier (URL, queue
    /// name) the UI can deep-link to.
    RestrictedArchive { ticket_template: String },
    /// Permanently deleted; only the deletion-authority reference
    /// remains. `deletion_id` lets auditors confirm the deletion in
    /// the operator's deletion log.
    PermanentlyDeleted {
        /// Deletion authority.
        deletion_authority: String,
        /// Deletion id.
        deletion_id: String,
    },
    /// In-process retrieval via an authenticated API endpoint. `scope`
    /// names the OAuth/JWT scope required.
    AuthenticatedApi { endpoint: String, scope: String },
}

impl ChainOfCustody {
    /// Construct a chain-of-custody record. `suppression_timestamp` is
    /// taken from `clock.now_rfc3339()` — pass `&WallClock` for
    /// live-blocker/runtime-error contexts, or a `&FrozenClock` derived
    /// from the package's intake hash for emit-pipeline contexts so the
    /// record enters the byte-reproducible BagIt manifest unchanged
    /// across re-emissions. Use `with_commitment` to attach the BLAKE3
    /// commitment over the suppressed payload.
    pub fn new(
        suppression_class: SuppressionClass,
        suppressing_component: impl Into<String>,
        policy_rule_id: impl Into<PolicyRuleId>,
        auditor_access: AuditorProcedure,
        clock: &dyn crate::clock::Clock,
    ) -> Self {
        Self {
            suppression_class,
            suppressing_component: suppressing_component.into(),
            suppression_timestamp: clock.now_rfc3339(),
            policy_rule_id: policy_rule_id.into(),
            cryptographic_commitment: None,
            auditor_access,
        }
    }

    /// Attach a BLAKE3 commitment over `payload`. Returns `self` so
    /// the construction reads as a builder chain.
    pub fn with_commitment(mut self, payload: &[u8]) -> Self {
        let hash = blake3::hash(payload);
        self.cryptographic_commitment = Some(ContentCommitment {
            algorithm: "blake3".into(),
            digest_hex: hash.to_hex().to_string(),
        });
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_minimal() {
        let c = ChainOfCustody::new(
            SuppressionClass::PhiStrict,
            "provenance_tiers::redact_record",
            "phi_strict:rule_1",
            AuditorProcedure::RestrictedArchive {
                ticket_template: "https://audit.example.org/ticket/{id}".into(),
            },
            &crate::clock::WallClock,
        );
        let json = serde_json::to_string(&c).unwrap();
        let back: ChainOfCustody = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn with_commitment_attaches_blake3_digest() {
        let c = ChainOfCustody::new(
            SuppressionClass::PromptInjection,
            "ingestion_safety::quarantine",
            "prompt_injection:rule_2",
            AuditorProcedure::PermanentlyDeleted {
                deletion_authority: "operator-fleet-1".into(),
                deletion_id: "del-2026-05-11-a".into(),
            },
            &crate::clock::WallClock,
        )
        .with_commitment(b"sensitive payload");
        let cc = c.cryptographic_commitment.as_ref().unwrap();
        assert_eq!(cc.algorithm, "blake3");
        // 32 bytes → 64 hex chars.
        assert_eq!(cc.digest_hex.len(), 64);
    }

    #[test]
    fn auditor_authenticated_api_round_trips() {
        let c = ChainOfCustody::new(
            SuppressionClass::ControlledAccessGenotype,
            "decision_substrate::record",
            "dbgap:rule_3",
            AuditorProcedure::AuthenticatedApi {
                endpoint: "https://archive.example.org/v1/retrieve".into(),
                scope: "auditor:read-restricted".into(),
            },
            &crate::clock::WallClock,
        );
        let json = serde_json::to_string(&c).unwrap();
        let back: ChainOfCustody = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn commitment_is_deterministic_for_same_payload() {
        let payload = b"identical bytes";
        let a = ChainOfCustody::new(
            SuppressionClass::RawCredential,
            "x",
            "y",
            AuditorProcedure::PermanentlyDeleted {
                deletion_authority: "a".into(),
                deletion_id: "b".into(),
            },
            &crate::clock::WallClock,
        )
        .with_commitment(payload);
        let b = ChainOfCustody::new(
            SuppressionClass::RawCredential,
            "x",
            "y",
            AuditorProcedure::PermanentlyDeleted {
                deletion_authority: "a".into(),
                deletion_id: "b".into(),
            },
            &crate::clock::WallClock,
        )
        .with_commitment(payload);
        assert_eq!(
            a.cryptographic_commitment.as_ref().map(|c| &c.digest_hex),
            b.cryptographic_commitment.as_ref().map(|c| &c.digest_hex)
        );
    }
}
