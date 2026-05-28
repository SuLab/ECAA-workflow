//! Runtime SWFC_ABLATE_* env-var flag readers.
//!
//! The closed `AblationFlag` enum + `all_flags()` are canonical in
//! `scripps-workflow-ecaa-types::ablation`. The `is_active()` extension
//! trait lives here to keep env-var coupling in core. Existing call
//! sites using `Foo.is_active()` method syntax keep working iff they
//! `use scripps_workflow_core::ablation::AblationFlagExt;` (or any
//! glob-import that pulls the trait into scope).

pub use scripps_workflow_ecaa_types::ablation::{all_flags, AblationFlag};

/// Extension trait providing the env-var-coupled `is_active()` check
/// outside ecaa-types (which is intentionally env-free / std-only).
pub trait AblationFlagExt {
    // `self` (by value) is the right convention here — `AblationFlag` is
    // `Copy` and the lint's preference for `&self` is cosmetic.
    #[allow(clippy::wrong_self_convention)]
    /// Is active.
    fn is_active(self) -> bool;
}

impl AblationFlagExt for AblationFlag {
    fn is_active(self) -> bool {
        crate::env_helpers::env_bool(self.env_var())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env_helpers::is_truthy;
    use std::env;

    #[test]
    fn all_six_flags_enumerated() {
        let flags = all_flags();
        assert_eq!(flags.len(), 6);
        let env_vars: Vec<_> = flags.iter().map(|f| f.env_var()).collect();
        assert!(env_vars.contains(&"SWFC_ABLATE_DECISION_RECORDS"));
        assert!(env_vars.contains(&"SWFC_ABLATE_AMENDMENT_PROVENANCE"));
        assert!(env_vars.contains(&"SWFC_ABLATE_CLAIM_CONSISTENCY"));
        assert!(env_vars.contains(&"SWFC_ABLATE_TYPED_BLOCKERS"));
        assert!(env_vars.contains(&"SWFC_ABLATE_REEXECUTION_CLASS"));
        assert!(env_vars.contains(&"SWFC_ABLATE_AUDIT_PROOF"));
    }

    #[test]
    fn truthy_values_recognized() {
        for v in ["1", "true", "True", "TRUE", "yes", "on"] {
            assert!(is_truthy(v), "expected truthy: {v}");
        }
        for v in ["0", "false", "no", "off", ""] {
            assert!(!is_truthy(v), "expected falsy: {v}");
        }
    }

    #[test]
    #[serial_test::serial]
    fn flag_is_active_reads_env_var() {
        let prev = env::var("SWFC_ABLATE_DECISION_RECORDS").ok();
        env::set_var("SWFC_ABLATE_DECISION_RECORDS", "1");
        assert!(AblationFlag::DecisionRecords.is_active());
        env::remove_var("SWFC_ABLATE_DECISION_RECORDS");
        assert!(!AblationFlag::DecisionRecords.is_active());
        if let Some(v) = prev {
            env::set_var("SWFC_ABLATE_DECISION_RECORDS", v);
        }
    }
}
