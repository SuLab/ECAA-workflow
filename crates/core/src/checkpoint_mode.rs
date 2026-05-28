//! Project-level checkpoint discipline.
//!
//! - Gated (default): every stage flagged `requires_sme_review: true`
//!   fires a `PendingConfirmation`.
//! - Selective: only stages flagged `checkpoint_level: required` stop;
//!   recommended-level stages auto-advance with an `AutoAdvanced`
//!   decision record.
//! - Fast: all checkpoints auto-advance.
//!
//! Locked at first Confirmation (mirrors `SessionMode`).
//! Confirmatory + Fast is rejected — a confirmatory analysis cannot
//! auto-advance prespecified stages.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    Hash,
    TS,
    Default,
    schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// CheckpointMode discriminant.
pub enum CheckpointMode {
    /// Fast variant.
    Fast,
    #[default]
    /// Gated variant.
    Gated,
    /// Selective variant.
    Selective,
}

/// Stage-level checkpoint-level override. Taxonomy YAML field
/// `checkpoint_level: "required" | "recommended"`. Absent = `Required`
/// so existing taxonomies behave identically under Gated (the only
/// supported mode before this field existed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointLevel {
    /// Required variant.
    Required,
    /// Recommended variant.
    Recommended,
}

impl CheckpointLevel {
    /// From opt str.
    pub fn from_opt_str(s: Option<&str>) -> Self {
        match s.map(str::trim) {
            Some("recommended") => Self::Recommended,
            _ => Self::Required,
        }
    }
}

impl CheckpointMode {
    /// Should the scheduler auto-advance past this stage? Fast skips
    /// everything; Selective skips `Recommended` stages only; Gated
    /// never skips. The `requires_sme_review` flag on the stage gates
    /// the checkpoint into existence at all — stages without the flag
    /// never pause regardless of mode.
    pub fn auto_advances(&self, requires_sme_review: bool) -> bool {
        self.auto_advances_level(requires_sme_review, CheckpointLevel::Required)
    }

    /// Variant that honors the per-stage `checkpoint_level` field.
    /// Callers with the full `StageSpec` should prefer this.
    pub fn auto_advances_level(&self, requires_sme_review: bool, level: CheckpointLevel) -> bool {
        if !requires_sme_review {
            // No checkpoint exists on this stage under any mode.
            return true;
        }
        match self {
            Self::Fast => true,
            Self::Gated => false,
            Self::Selective => matches!(level, CheckpointLevel::Recommended),
        }
    }

    /// Stable wire string used in `DecisionType::AutoAdvanced::mode`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Gated => "gated",
            Self::Selective => "selective",
        }
    }

    /// Runtime gate: is this checkpoint mode compatible with a
    /// confirmatory analysis discipline?
    ///
    /// Mirrors the confirm-time gate in
    /// `service/transitions.rs::confirm_with_options`. Anywhere the
    /// scheduler has both a `SessionMode::Confirmatory` and a
    /// `CheckpointMode` in scope (e.g. an emitter post-load consistency
    /// check, a future `set_checkpoint_mode` mutation tool), this is
    /// the canonical predicate to consult — Fast cannot coexist with
    /// Confirmatory because a confirmatory session cannot auto-advance
    /// prespecified stages.
    ///
    /// Returns `Ok(())` for any compatible combination, otherwise an
    /// error string suitable for surfacing to the SME.
    pub fn ensure_compatible_with_confirmatory(
        &self,
        is_confirmatory: bool,
    ) -> Result<(), &'static str> {
        if is_confirmatory && matches!(self, Self::Fast) {
            return Err("confirmatory + fast is rejected — \
                 a confirmatory session cannot auto-advance prespecified stages");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_gated() {
        assert_eq!(CheckpointMode::default(), CheckpointMode::Gated);
    }

    #[test]
    fn fast_auto_advances_everything() {
        assert!(CheckpointMode::Fast.auto_advances(true));
        assert!(CheckpointMode::Fast.auto_advances(false));
    }

    #[test]
    fn gated_never_auto_advances_a_required_stage() {
        assert!(!CheckpointMode::Gated.auto_advances(true));
        // Stages without the review flag never pause.
        assert!(CheckpointMode::Gated.auto_advances(false));
    }

    #[test]
    fn selective_honors_checkpoint_level() {
        // Required stage: still pauses.
        assert!(!CheckpointMode::Selective.auto_advances_level(true, CheckpointLevel::Required));
        // Recommended stage: auto-advances.
        assert!(CheckpointMode::Selective.auto_advances_level(true, CheckpointLevel::Recommended));
        // No review flag: always auto-advances.
        assert!(CheckpointMode::Selective.auto_advances(false));
    }

    #[test]
    fn checkpoint_level_parses_lenient() {
        assert_eq!(
            CheckpointLevel::from_opt_str(Some("recommended")),
            CheckpointLevel::Recommended
        );
        assert_eq!(
            CheckpointLevel::from_opt_str(Some("required")),
            CheckpointLevel::Required
        );
        // Unknown + None both fall back to the conservative default.
        assert_eq!(
            CheckpointLevel::from_opt_str(None),
            CheckpointLevel::Required
        );
        assert_eq!(
            CheckpointLevel::from_opt_str(Some("")),
            CheckpointLevel::Required
        );
    }

    #[test]
    fn confirmatory_fast_runtime_gate_rejects() {
        // The runtime gate mirrors the confirm-time guard so any future
        // surface that accepts a CheckpointMode without going through the
        // confirm path can defend the invariant. (S5.14)
        assert!(CheckpointMode::Fast
            .ensure_compatible_with_confirmatory(true)
            .is_err());
        assert!(CheckpointMode::Gated
            .ensure_compatible_with_confirmatory(true)
            .is_ok());
        assert!(CheckpointMode::Selective
            .ensure_compatible_with_confirmatory(true)
            .is_ok());
        // Exploratory (is_confirmatory == false) accepts every mode.
        for m in [
            CheckpointMode::Fast,
            CheckpointMode::Gated,
            CheckpointMode::Selective,
        ] {
            assert!(m.ensure_compatible_with_confirmatory(false).is_ok());
        }
    }

    #[test]
    fn serde_round_trip() {
        for (mode, wire) in [
            (CheckpointMode::Fast, "\"fast\""),
            (CheckpointMode::Gated, "\"gated\""),
            (CheckpointMode::Selective, "\"selective\""),
        ] {
            let json = serde_json::to_string(&mode).unwrap();
            assert_eq!(json, wire);
            let back: CheckpointMode = serde_json::from_str(&json).unwrap();
            assert_eq!(mode, back);
        }
    }
}
