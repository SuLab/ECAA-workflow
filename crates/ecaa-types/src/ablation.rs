//! AblationFlag enum + all_flags() — the 6-variant SWFC_ABLATE_* contract.
//! The is_active() runtime check is in scripps-workflow-core::ablation
//! as an extension trait `AblationFlagExt` to keep env-var coupling there.

#[derive(Debug, Clone, Copy, PartialEq, Eq, schemars::JsonSchema)]
pub enum AblationFlag {
    DecisionRecords,
    AmendmentProvenance,
    ClaimConsistency,
    TypedBlockers,
    ReexecutionClass,
    AuditProof,
}

impl AblationFlag {
    pub fn env_var(self) -> &'static str {
        match self {
            Self::DecisionRecords => "SWFC_ABLATE_DECISION_RECORDS",
            Self::AmendmentProvenance => "SWFC_ABLATE_AMENDMENT_PROVENANCE",
            Self::ClaimConsistency => "SWFC_ABLATE_CLAIM_CONSISTENCY",
            Self::TypedBlockers => "SWFC_ABLATE_TYPED_BLOCKERS",
            Self::ReexecutionClass => "SWFC_ABLATE_REEXECUTION_CLASS",
            Self::AuditProof => "SWFC_ABLATE_AUDIT_PROOF",
        }
    }
}

pub fn all_flags() -> [AblationFlag; 6] {
    [
        AblationFlag::DecisionRecords,
        AblationFlag::AmendmentProvenance,
        AblationFlag::ClaimConsistency,
        AblationFlag::TypedBlockers,
        AblationFlag::ReexecutionClass,
        AblationFlag::AuditProof,
    ]
}
