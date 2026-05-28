//! Session-level discipline flag: exploratory vs. confirmatory vs.
//! hybrid. Locked at first Confirmation; cannot change post-emission.
//!
//! Current wire shape is stage-level only (`prespecified_stages: Vec<String>`).
//! The Rust types are forward-compatible for parameter-level prespecification
//! landing later via a parallel `prespecified_parameters` field that
//! doesn't break existing sessions.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// StageMode discriminant.
pub enum StageMode {
    /// Exploratory variant.
    Exploratory,
    /// Confirmatory variant.
    Confirmatory,
}

#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
/// SessionMode discriminant.
pub enum SessionMode {
    /// Default. SME may add, remove, reorder modules; method comparisons
    /// allowed; deviations are not tagged as post-hoc.
    #[default]
    Exploratory,
    /// SAP-style discipline. `prespecified_stages` names the stages that
    /// carry the prespec contract. Amendments to a prespecified stage
    /// require a non-empty rationale and write a `PostHocDeviation`
    /// record.
    ///
    /// `prespecified_parameters` is a forward-compat placeholder for
    /// parameter-level prespecification. `#[serde(skip)]` today so the
    /// wire shape stays purely stage-level, `#[ts(skip)]` so it doesn't
    /// leak into the UI type bindings. Adding parameter-level later
    /// just flips the attribute off and regenerates types — no
    /// breaking change.
    Confirmatory {
        #[serde(default)]
        /// Prespecified stages.
        prespecified_stages: Vec<String>,
        #[serde(skip)]
        #[ts(skip)]
        /// Prespecified parameters.
        prespecified_parameters: BTreeMap<String, Vec<String>>,
    },
    /// Per-stage mode map. Confirmatory + exploratory can coexist in the
    /// same session (e.g. confirmatory primary endpoint + exploratory
    /// biomarker secondary).
    Hybrid {
        #[serde(default)]
        /// Stage modes.
        stage_modes: BTreeMap<String, StageMode>,
    },
}

impl SessionMode {
    /// Does `stage` require the `PostHocDeviation` gate if amended?
    /// True only when the stage is explicitly prespecified in
    /// Confirmatory, or marked `Confirmatory` in Hybrid's stage map.
    pub fn is_prespecified(&self, stage: &str) -> bool {
        match self {
            Self::Exploratory => false,
            Self::Confirmatory {
                prespecified_stages,
                ..
            } => prespecified_stages.iter().any(|s| s == stage),
            Self::Hybrid { stage_modes } => {
                matches!(stage_modes.get(stage), Some(StageMode::Confirmatory))
            }
        }
    }

    /// Any confirmatory discipline active on this session? Drives the
    /// "Confirmatory" UI badge + the CheckpointMode::Fast precondition.
    pub fn is_confirmatory(&self) -> bool {
        match self {
            Self::Exploratory => false,
            Self::Confirmatory { .. } => true,
            Self::Hybrid { stage_modes } => stage_modes
                .values()
                .any(|m| matches!(m, StageMode::Confirmatory)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_exploratory() {
        assert_eq!(SessionMode::default(), SessionMode::Exploratory);
    }

    #[test]
    fn exploratory_has_no_prespecified_stages() {
        let mode = SessionMode::Exploratory;
        assert!(!mode.is_prespecified("primary_endpoint"));
        assert!(!mode.is_confirmatory());
    }

    #[test]
    fn confirmatory_gates_prespecified_stages_only() {
        let mode = SessionMode::Confirmatory {
            prespecified_stages: vec!["primary_endpoint".into(), "population_definition".into()],
            prespecified_parameters: BTreeMap::new(),
        };
        assert!(mode.is_prespecified("primary_endpoint"));
        assert!(mode.is_prespecified("population_definition"));
        assert!(!mode.is_prespecified("subgroup_analyses"));
        assert!(mode.is_confirmatory());
    }

    #[test]
    fn hybrid_honors_stage_map() {
        let mut stage_modes = BTreeMap::new();
        stage_modes.insert("primary_endpoint".into(), StageMode::Confirmatory);
        stage_modes.insert("biomarker_exploration".into(), StageMode::Exploratory);
        let mode = SessionMode::Hybrid { stage_modes };
        assert!(mode.is_prespecified("primary_endpoint"));
        assert!(!mode.is_prespecified("biomarker_exploration"));
        assert!(mode.is_confirmatory());
    }

    #[test]
    fn hybrid_all_exploratory_is_not_confirmatory() {
        let mut stage_modes = BTreeMap::new();
        stage_modes.insert("s1".into(), StageMode::Exploratory);
        let mode = SessionMode::Hybrid { stage_modes };
        assert!(!mode.is_confirmatory());
    }

    #[test]
    fn serde_round_trip_stage_level() {
        let mode = SessionMode::Confirmatory {
            prespecified_stages: vec!["primary_endpoint".into()],
            prespecified_parameters: BTreeMap::new(),
        };
        let json = serde_json::to_string(&mode).unwrap();
        assert!(json.contains("\"kind\":\"confirmatory\""));
        assert!(json.contains("prespecified_stages"));
        // Per D5 forward-compat: the parameter-level field MUST NOT
        // appear on the wire today (serde(skip) placeholder).
        assert!(!json.contains("prespecified_parameters"));
        let back: SessionMode = serde_json::from_str(&json).unwrap();
        assert_eq!(mode, back);
    }

    #[test]
    fn prespecified_parameters_placeholder_survives_round_trip() {
        // Deserialize a minimal wire shape (no parameters) and verify
        // the field loads as an empty map. This guards against a
        // future edit that accidentally flips the #[serde(skip)] off
        // without adding migration logic.
        let wire = r#"{"kind":"confirmatory","prespecified_stages":["p"]}"#;
        let mode: SessionMode = serde_json::from_str(wire).unwrap();
        match mode {
            SessionMode::Confirmatory {
                prespecified_stages,
                prespecified_parameters,
            } => {
                assert_eq!(prespecified_stages, vec!["p".to_string()]);
                assert!(prespecified_parameters.is_empty());
            }
            _ => panic!("expected confirmatory"),
        }
    }

    #[test]
    fn legacy_sessions_default_to_exploratory() {
        // Legacy JSON has no `mode` field. The `#[serde(default)]` on the
        // Session field falls back to Exploratory. We test the enum
        // directly here; the Session-level test lives in the conversation
        // crate.
        #[derive(Deserialize)]
        struct Holder {
            #[serde(default)]
            mode: SessionMode,
        }
        let h: Holder = serde_json::from_str("{}").unwrap();
        assert_eq!(h.mode, SessionMode::Exploratory);
    }
}
