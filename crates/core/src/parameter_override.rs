//! SME-authored per-task parameter overrides. These are *SME* inputs (never
//! LLM recommendations) that bind a concrete value to an atom's declared
//! `ParameterSpec`, lowered onto `Task.spec["sme_parameter_overrides"]` so the
//! execution agent applies them. Enforcement of the *result* is a separate
//! concern (validation contracts, see the harness `run_assertion`).
//!
//! Deterministic-emit invariant: the map is `BTreeMap`-backed at both levels so
//! serialization is byte-stable regardless of insertion order.

use crate::atom::{ParameterSpec, ParameterType};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Where a per-task parameter override came from. Only `Sme` today; the enum
/// exists so an audit reader can distinguish an SME-mandated value from any
/// future machine-derived source. `#[non_exhaustive]` keeps adding a source a
/// non-breaking minor change for ts-rs / RO-Crate consumers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum OverrideSource {
    /// The SME set this value explicitly (button / structured editor).
    Sme,
}

/// One concrete value bound to a named atom parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[ts(export)]
pub struct SmeParameterOverride {
    /// The concrete value the agent must apply verbatim.
    #[ts(type = "unknown")]
    pub value: serde_json::Value,
    /// Provenance of this override.
    pub source: OverrideSource,
}

/// Per-task parameter overrides: `task_id -> (param name -> override)`.
/// `BTreeMap` at both levels for byte-deterministic emission.
#[derive(
    Debug, Clone, Default, PartialEq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[ts(export)]
pub struct ParameterOverrides(pub BTreeMap<String, BTreeMap<String, SmeParameterOverride>>);

/// Validation failures when an SME override is checked against the atom's
/// declared `ParameterSpec`. Not wire-facing (never serialized), so no
/// `#[non_exhaustive]` needed — it surfaces only as an emit-time error string.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ParamOverrideError {
    /// The parameter name is not declared by the atom.
    #[error("parameter `{param}` is not declared by task `{task_id}`")]
    UnknownParameter {
        /// Task the override targeted.
        task_id: String,
        /// The offending parameter name.
        param: String,
    },
    /// The value is not in the atom's closed `allowed_values` set.
    #[error("value for `{param}` on task `{task_id}` is not in the allowed set")]
    NotAllowed {
        /// Task the override targeted.
        task_id: String,
        /// The offending parameter name.
        param: String,
    },
    /// The value's JSON type does not match the declared `ParameterType`.
    #[error("value for `{param}` on task `{task_id}` does not match declared type {expected:?}")]
    TypeMismatch {
        /// Task the override targeted.
        task_id: String,
        /// The offending parameter name.
        param: String,
        /// The declared type the value failed to match.
        expected: ParameterType,
    },
}

impl ParameterOverrides {
    /// True when no task carries any override.
    pub fn is_empty(&self) -> bool {
        self.0.values().all(|m| m.is_empty())
    }

    /// Set (or replace) one task's parameter value.
    pub fn set(
        &mut self,
        task_id: &str,
        param: &str,
        value: serde_json::Value,
        source: OverrideSource,
    ) {
        self.0
            .entry(task_id.to_string())
            .or_default()
            .insert(param.to_string(), SmeParameterOverride { value, source });
    }

    /// Remove one task's parameter override (no-op when absent).
    pub fn remove(&mut self, task_id: &str, param: &str) {
        if let Some(m) = self.0.get_mut(task_id) {
            m.remove(param);
        }
    }

    /// The non-empty override map for one task, if any.
    pub fn for_task(&self, task_id: &str) -> Option<&BTreeMap<String, SmeParameterOverride>> {
        self.0.get(task_id).filter(|m| !m.is_empty())
    }

    /// Validate this task's overrides against the atom's declared parameters.
    /// Fail-closed: an unknown parameter name, an out-of-`allowed_values`
    /// value, or a type mismatch is an error the emit path must surface.
    pub fn validate_against(
        &self,
        task_id: &str,
        specs: &[ParameterSpec],
    ) -> Result<(), ParamOverrideError> {
        let Some(overrides) = self.0.get(task_id) else {
            return Ok(());
        };
        for (name, ov) in overrides {
            let spec = specs.iter().find(|s| &s.name == name).ok_or_else(|| {
                ParamOverrideError::UnknownParameter {
                    task_id: task_id.into(),
                    param: name.clone(),
                }
            })?;
            if !spec.allowed_values.is_empty() && !spec.allowed_values.contains(&ov.value) {
                return Err(ParamOverrideError::NotAllowed {
                    task_id: task_id.into(),
                    param: name.clone(),
                });
            }
            if !type_matches(&spec.r#type, &ov.value) {
                return Err(ParamOverrideError::TypeMismatch {
                    task_id: task_id.into(),
                    param: name.clone(),
                    expected: spec.r#type,
                });
            }
        }
        Ok(())
    }
}

/// True when the JSON value is compatible with the declared parameter type.
/// Unknown (future) `ParameterType` kinds pass on type only — `allowed_values`
/// still gates them when non-empty.
fn type_matches(t: &ParameterType, v: &serde_json::Value) -> bool {
    use ParameterType::*;
    match t {
        String | Enum => v.is_string(),
        Number => v.is_number(),
        Integer => v.is_i64() || v.is_u64(),
        Boolean => v.is_boolean(),
        Array => v.is_array(),
        Object => v.is_object(),
        // `#[non_exhaustive]` ParameterType: fail-open on type only.
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::{ParameterSpec, ParameterType};
    use serde_json::json;

    fn spec_enum(name: &str, allowed: &[&str]) -> ParameterSpec {
        ParameterSpec {
            name: name.into(),
            r#type: ParameterType::Enum,
            required: false,
            default: None,
            allowed_values: allowed.iter().map(|s| json!(s)).collect(),
            examples: vec![],
            description: None,
        }
    }

    fn spec_int(name: &str) -> ParameterSpec {
        ParameterSpec {
            name: name.into(),
            r#type: ParameterType::Integer,
            required: false,
            default: None,
            allowed_values: vec![],
            examples: vec![],
            description: None,
        }
    }

    #[test]
    fn rejects_value_outside_allowed_values() {
        let mut ov = ParameterOverrides::default();
        ov.set("align", "aligner", json!("bowtie2"), OverrideSource::Sme);
        let err = ov
            .validate_against("align", &[spec_enum("aligner", &["star", "hisat2"])])
            .unwrap_err();
        assert!(matches!(err, ParamOverrideError::NotAllowed { .. }));
    }

    #[test]
    fn accepts_value_in_allowed_values_and_is_byte_deterministic() {
        let mut ov = ParameterOverrides::default();
        ov.set("align", "aligner", json!("star"), OverrideSource::Sme);
        ov.validate_against("align", &[spec_enum("aligner", &["star", "hisat2"])])
            .unwrap();
        // BTreeMap ordering => stable serialization.
        assert_eq!(
            serde_json::to_string(&ov).unwrap(),
            serde_json::to_string(&ov.clone()).unwrap()
        );
    }

    #[test]
    fn rejects_unknown_parameter_name() {
        let mut ov = ParameterOverrides::default();
        ov.set("align", "nonexistent", json!(1), OverrideSource::Sme);
        assert!(matches!(
            ov.validate_against("align", &[spec_enum("aligner", &["star"])])
                .unwrap_err(),
            ParamOverrideError::UnknownParameter { .. }
        ));
    }

    #[test]
    fn rejects_type_mismatch() {
        let mut ov = ParameterOverrides::default();
        ov.set("align", "min_mapq", json!("twenty"), OverrideSource::Sme);
        assert!(matches!(
            ov.validate_against("align", &[spec_int("min_mapq")])
                .unwrap_err(),
            ParamOverrideError::TypeMismatch { .. }
        ));
    }

    #[test]
    fn accepts_integer_value_for_integer_spec() {
        let mut ov = ParameterOverrides::default();
        ov.set("align", "min_mapq", json!(20), OverrideSource::Sme);
        ov.validate_against("align", &[spec_int("min_mapq")]).unwrap();
        assert!(ov.for_task("align").is_some());
        assert!(ov.for_task("other").is_none());
    }

    #[test]
    fn empty_when_no_overrides() {
        let ov = ParameterOverrides::default();
        assert!(ov.is_empty());
        // Unknown task validates trivially (no overrides for it).
        ov.validate_against("align", &[]).unwrap();
    }
}
