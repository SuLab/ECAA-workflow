//! SME-authored per-stage validation bounds. These are *SME* output
//! constraints (never LLM recommendations) — a p-value cutoff, a fold-change
//! floor, an artifact-presence requirement — that merge into the emitted
//! `policies/validation-contract.json` and are enforced post-hoc by the harness
//! `run_assertion` / `enforce_validation_contract`, which recompute from
//! `result.json` and re-block on violation. This preserves method-neutrality:
//! the SME sets the *constraint on the result*, not the method that produces it.
//!
//! `assertion_type` is restricted to the set the harness actually implements
//! (`SUPPORTED_ASSERTION_TYPES`, mirrored from `run_assertion`'s match) — an
//! unimplemented type would make `run_assertion` fail-closed to `false` and
//! permanently block a legitimate run, so `merge_into_contract` drops it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Assertion types the harness `run_assertion` (`crates/harness/src/main.rs`)
/// actually implements. Kept in sync with that `match`. An SME bound whose
/// `assertion_type` is not in this set is rejected / dropped (fail-closed): an
/// unimplemented type resolves to `false` in `run_assertion` and would block
/// every run regardless of the result.
pub const SUPPORTED_ASSERTION_TYPES: &[&str] = &[
    "artifact_present",
    "artifact_non_empty_table",
    "artifact_glob_any",
    "string_contains",
    "numeric_threshold",
    "numeric_distribution",
    "reference_range_outlier",
    "positive_control_present",
    "negative_control_present",
    "json_pointer_is_bool",
    "json_pointer_is_array",
    "cross_stage_output_comparison",
    "cross_field_equals",
    "formula_references_covariates",
];

/// True when `assertion_type` is one the harness can actually evaluate.
pub fn is_supported_assertion_type(assertion_type: &str) -> bool {
    SUPPORTED_ASSERTION_TYPES.contains(&assertion_type)
}

/// Severities the merged contract + harness understand. `required` blocks on
/// violation; `recommended` warns.
pub const SUPPORTED_SEVERITIES: &[&str] = &["required", "recommended"];

/// True when `severity` is one the contract enforcement understands.
pub fn is_valid_severity(severity: &str) -> bool {
    SUPPORTED_SEVERITIES.contains(&severity)
}

/// Validate that a bound's `check` payload carries the fields the harness
/// `run_assertion` (`crates/harness/src/main.rs`) actually reads for
/// `assertion_type`. A supported type with a missing/typo'd check field makes
/// `run_assertion` fail-closed to `false` forever — a required bound then
/// permanently re-blocks the stage. Rejecting the malformed shape at set-time
/// turns that silent permanent-block into a clean 400.
///
/// The field lists mirror the `?`-early-return reads in each `run_assertion`
/// match arm. `Ok(())` for a well-shaped (or check-free) assertion; `Err(msg)`
/// naming the missing field(s). Assumes `assertion_type` is already known to be
/// supported (call `is_supported_assertion_type` first); an unsupported type
/// returns an error naming it.
pub fn validate_bound_check_shape(assertion_type: &str, check: &Value) -> Result<(), String> {
    let obj = check.as_object();
    // A string field present + non-null. `run_assertion` reads these via
    // `.and_then(|v| v.as_str())`.
    let has_str = |k: &str| {
        obj.and_then(|o| o.get(k))
            .and_then(|v| v.as_str())
            .is_some()
    };
    // A numeric field readable as f64 (matches `.as_f64()`).
    let has_num = |k: &str| {
        obj.and_then(|o| o.get(k))
            .and_then(|v| v.as_f64())
            .is_some()
    };
    let has_array = |k: &str| {
        obj.and_then(|o| o.get(k))
            .and_then(|v| v.as_array())
            .is_some()
    };
    // Assemble an error listing every missing field so the SME fixes them all
    // in one round-trip instead of one 400 per field.
    let require = |missing: Vec<&str>| -> Result<(), String> {
        if missing.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "assertion_type `{assertion_type}` requires check field(s): {}",
                missing.join(", ")
            ))
        }
    };
    match assertion_type {
        // Presence-style: no `check` fields consumed by run_assertion.
        "artifact_present" | "artifact_non_empty_table" | "artifact_glob_any" => Ok(()),
        // Needs one of `substrings` / `substrings_any` (array); else false.
        "string_contains" => {
            if has_array("substrings") || has_array("substrings_any") {
                Ok(())
            } else {
                Err(format!(
                    "assertion_type `{assertion_type}` requires check.substrings or \
                     check.substrings_any (a non-empty array of strings)"
                ))
            }
        }
        "numeric_threshold" => {
            let mut missing = Vec::new();
            if !has_str("json_pointer") {
                missing.push("json_pointer (string)");
            }
            if !has_str("op") {
                missing.push("op (string)");
            }
            if !has_num("value") {
                missing.push("value (number)");
            }
            require(missing)
        }
        "numeric_distribution" => {
            let mut missing = Vec::new();
            if !has_str("json_pointer") {
                missing.push("json_pointer (string)");
            }
            if !has_str("stat") {
                missing.push("stat (string)");
            }
            if !has_str("op") {
                missing.push("op (string)");
            }
            if !has_num("value") {
                missing.push("value (number)");
            }
            require(missing)
        }
        "reference_range_outlier" => {
            let mut missing = Vec::new();
            if !has_str("json_pointer") {
                missing.push("json_pointer (string)");
            }
            if !has_num("reference_min") {
                missing.push("reference_min (number)");
            }
            if !has_num("reference_max") {
                missing.push("reference_max (number)");
            }
            require(missing)
        }
        "positive_control_present"
        | "negative_control_present"
        | "json_pointer_is_bool"
        | "json_pointer_is_array" => {
            if has_str("json_pointer") {
                Ok(())
            } else {
                require(vec!["json_pointer (string)"])
            }
        }
        "cross_stage_output_comparison" => {
            let mut missing = Vec::new();
            if !has_str("this_pointer") {
                missing.push("this_pointer (string)");
            }
            if !has_str("upstream_task") {
                missing.push("upstream_task (string)");
            }
            if !has_str("upstream_pointer") {
                missing.push("upstream_pointer (string)");
            }
            if !has_str("op") {
                missing.push("op (string)");
            }
            require(missing)
        }
        "cross_field_equals" => {
            let mut missing = Vec::new();
            if !has_str("this_pointer") {
                missing.push("this_pointer (string)");
            }
            if !has_str("other_pointer") {
                missing.push("other_pointer (string)");
            }
            require(missing)
        }
        "formula_references_covariates" => {
            let mut missing = Vec::new();
            if !has_str("formula_pointer") {
                missing.push("formula_pointer (string)");
            }
            if !has_str("covariates_pointer") {
                missing.push("covariates_pointer (string)");
            }
            if !has_str("primary_pointer") {
                missing.push("primary_pointer (string)");
            }
            require(missing)
        }
        other => Err(format!("assertion_type `{other}` is not harness-runnable")),
    }
}

/// One SME-authored validation bound, shaped so it lowers directly into a
/// validation-contract `stages.<stage_class>.assertions[]` entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[ts(export)]
pub struct SmeValidationBound {
    /// Stage class the bound applies to (contract key `stages.<stage_class>`).
    pub stage_class: String,
    /// Assertion type — must be one of `SUPPORTED_ASSERTION_TYPES`.
    pub assertion_type: String,
    /// Relative artifact path the assertion reads (e.g. `results/tables/de.json`).
    pub target: String,
    /// Type-specific check object (json_pointer/op/value, substrings, …).
    #[ts(type = "unknown")]
    pub check: Value,
    /// `required` (blocks) or `recommended` (warns).
    pub severity: String,
    /// Stable id — replaced idempotently when the same id is set again.
    pub id: String,
    /// Human description surfaced in the Decisions / Claims UI.
    pub description: String,
}

impl SmeValidationBound {
    /// True when this bound's `assertion_type` is harness-runnable.
    pub fn is_supported(&self) -> bool {
        is_supported_assertion_type(&self.assertion_type)
    }

    /// Lower this bound into the contract-assertion JSON object shape
    /// (`{id, description, assertion_type, target, check, severity}`). `check`
    /// is omitted when it is not an object (e.g. `artifact_present` needs none).
    fn to_assertion_object(&self) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("id".into(), Value::String(self.id.clone()));
        obj.insert(
            "description".into(),
            Value::String(self.description.clone()),
        );
        obj.insert(
            "assertion_type".into(),
            Value::String(self.assertion_type.clone()),
        );
        obj.insert("target".into(), Value::String(self.target.clone()));
        if self.check.is_object() {
            obj.insert("check".into(), self.check.clone());
        }
        obj.insert("severity".into(), Value::String(self.severity.clone()));
        Value::Object(obj)
    }
}

/// An ordered set of SME validation bounds. `Vec` (not a map) so authored order
/// is preserved deterministically into the merged contract.
#[derive(
    Debug, Clone, Default, PartialEq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[ts(export)]
pub struct SmeValidationBounds(pub Vec<SmeValidationBound>);

impl SmeValidationBounds {
    /// True when there are no bounds.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Every bound whose `assertion_type` the harness can evaluate.
    pub fn supported(&self) -> impl Iterator<Item = &SmeValidationBound> {
        self.0.iter().filter(|b| b.is_supported())
    }
}

/// Merge SME bounds into a validation contract, returning the merged document.
///
/// - `existing` is the archetype contract JSON, or `None` to synthesize a
///   fresh `{contract_id: "sme-authored", version: "0.1", stages: {}}`.
/// - Each supported bound is appended to `stages.<stage_class>.assertions`,
///   creating the stage/array as needed. When a bound's `id` already exists in
///   that stage, it is REPLACED in place (idempotent edit).
/// - Bounds whose `assertion_type` is not harness-runnable are dropped
///   (fail-closed — see `SUPPORTED_ASSERTION_TYPES`).
pub fn merge_into_contract(existing: Option<Value>, bounds: &SmeValidationBounds) -> Value {
    let mut contract = existing.unwrap_or_else(|| {
        serde_json::json!({
            "contract_id": "sme-authored",
            "version": "0.1",
            "stages": {},
        })
    });

    // Ensure the top-level object + `stages` object exist.
    if !contract.is_object() {
        contract = serde_json::json!({
            "contract_id": "sme-authored",
            "version": "0.1",
            "stages": {},
        });
    }
    let root = contract
        .as_object_mut()
        .expect("contract is an object by construction above");
    let stages = root
        .entry("stages")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !stages.is_object() {
        *stages = Value::Object(serde_json::Map::new());
    }
    let stages = stages
        .as_object_mut()
        .expect("stages is an object by construction above");

    for bound in bounds.supported() {
        let stage = stages
            .entry(bound.stage_class.clone())
            .or_insert_with(|| serde_json::json!({ "assertions": [] }));
        // A stage entry must be an object carrying an `assertions` array.
        if !stage.is_object() {
            *stage = serde_json::json!({ "assertions": [] });
        }
        let stage_obj = stage.as_object_mut().expect("stage is an object");
        let assertions = stage_obj
            .entry("assertions")
            .or_insert_with(|| Value::Array(Vec::new()));
        if !assertions.is_array() {
            *assertions = Value::Array(Vec::new());
        }
        let arr = assertions.as_array_mut().expect("assertions is an array");
        let new_obj = bound.to_assertion_object();
        // Idempotent: replace an assertion carrying the same id, else append.
        if let Some(slot) = arr.iter_mut().find(|a| a.get("id") == new_obj.get("id")) {
            *slot = new_obj;
        } else {
            arr.push(new_obj);
        }
    }

    contract
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bound(id: &str, stage: &str, atype: &str) -> SmeValidationBound {
        SmeValidationBound {
            stage_class: stage.into(),
            assertion_type: atype.into(),
            target: "results/tables/de.json".into(),
            check: serde_json::json!({ "json_pointer": "/adjusted_p_max", "op": "lte", "value": 0.01 }),
            severity: "required".into(),
            id: id.into(),
            description: "SME bound".into(),
        }
    }

    #[test]
    fn merge_adds_sme_bound_to_existing_stage() {
        let existing = serde_json::json!({
            "contract_id": "c", "version": "0.1",
            "stages": { "differential_expression": { "assertions": [] } }
        });
        let mut b = SmeValidationBounds::default();
        b.0.push(SmeValidationBound {
            stage_class: "differential_expression".into(),
            assertion_type: "numeric_threshold".into(),
            target: "results/tables/de.json".into(),
            check: serde_json::json!({ "json_pointer": "/adjusted_p_max", "op": "lte", "value": 0.01 }),
            severity: "required".into(),
            id: "sme_de_padj".into(),
            description: "SME: adjusted p must be <= 0.01".into(),
        });
        let merged = merge_into_contract(Some(existing), &b);
        let asserts = &merged["stages"]["differential_expression"]["assertions"];
        assert_eq!(asserts.as_array().unwrap().len(), 1);
        assert_eq!(asserts[0]["id"], "sme_de_padj");
        assert_eq!(asserts[0]["assertion_type"], "numeric_threshold");
        assert_eq!(asserts[0]["severity"], "required");
    }

    #[test]
    fn merge_synthesizes_contract_when_none() {
        let mut b = SmeValidationBounds::default();
        b.0.push(bound("sme_1", "quality_control", "numeric_threshold"));
        let merged = merge_into_contract(None, &b);
        assert_eq!(merged["contract_id"], "sme-authored");
        assert_eq!(merged["version"], "0.1");
        assert_eq!(
            merged["stages"]["quality_control"]["assertions"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn merge_replaces_bound_with_same_id_idempotently() {
        let mut b = SmeValidationBounds::default();
        b.0.push(bound("dup", "de", "numeric_threshold"));
        let once = merge_into_contract(None, &b);
        // Re-merge the same id into the prior result — must not duplicate.
        let twice = merge_into_contract(Some(once), &b);
        assert_eq!(
            twice["stages"]["de"]["assertions"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "same id must replace, not append"
        );
    }

    #[test]
    fn merge_drops_unsupported_assertion_type() {
        let mut b = SmeValidationBounds::default();
        // json_key_equals is present in the schema enum but NOT implemented by
        // the harness run_assertion match — must be dropped fail-closed.
        b.0.push(bound("bad", "de", "json_key_equals"));
        let merged = merge_into_contract(None, &b);
        assert!(
            merged["stages"].get("de").is_none(),
            "unsupported assertion type must not reach the contract"
        );
        assert!(!is_supported_assertion_type("json_key_equals"));
        assert!(is_supported_assertion_type("numeric_threshold"));
    }

    #[test]
    fn check_shape_accepts_well_formed_and_rejects_malformed() {
        use super::{is_valid_severity, validate_bound_check_shape};
        // Well-formed numeric_threshold.
        assert!(validate_bound_check_shape(
            "numeric_threshold",
            &serde_json::json!({ "json_pointer": "/p", "op": "lte", "value": 0.05 })
        )
        .is_ok());
        // Missing op + value → error naming both.
        let err = validate_bound_check_shape(
            "numeric_threshold",
            &serde_json::json!({ "json_pointer": "/p" }),
        )
        .unwrap_err();
        assert!(err.contains("op"), "error must name missing op: {err}");
        assert!(
            err.contains("value"),
            "error must name missing value: {err}"
        );
        // Presence-style needs no check.
        assert!(validate_bound_check_shape("artifact_present", &serde_json::Value::Null).is_ok());
        // string_contains needs substrings or substrings_any.
        assert!(validate_bound_check_shape(
            "string_contains",
            &serde_json::json!({ "substrings": ["x"] })
        )
        .is_ok());
        assert!(validate_bound_check_shape(
            "string_contains",
            &serde_json::json!({ "json_pointer": "/n" })
        )
        .is_err());
        // reference_range_outlier requires the range bounds.
        assert!(validate_bound_check_shape(
            "reference_range_outlier",
            &serde_json::json!({ "json_pointer": "/vals", "reference_min": 0.0, "reference_max": 1.0 })
        )
        .is_ok());
        assert!(validate_bound_check_shape(
            "reference_range_outlier",
            &serde_json::json!({ "json_pointer": "/vals" })
        )
        .is_err());
        // typed presence guards need json_pointer.
        assert!(validate_bound_check_shape(
            "json_pointer_is_bool",
            &serde_json::json!({ "json_pointer": "/converged" })
        )
        .is_ok());
        assert!(
            validate_bound_check_shape("json_pointer_is_bool", &serde_json::json!({})).is_err()
        );
        // severity gate.
        assert!(is_valid_severity("required"));
        assert!(is_valid_severity("recommended"));
        assert!(!is_valid_severity("blocking"));
        assert!(!is_valid_severity(""));
    }

    #[test]
    fn merge_omits_check_for_presence_style_assertion() {
        let mut b = SmeValidationBounds::default();
        b.0.push(SmeValidationBound {
            stage_class: "de".into(),
            assertion_type: "artifact_present".into(),
            target: "results/tables/de.json".into(),
            check: serde_json::Value::Null,
            severity: "required".into(),
            id: "present".into(),
            description: "file must exist".into(),
        });
        let merged = merge_into_contract(None, &b);
        let a = &merged["stages"]["de"]["assertions"][0];
        assert!(a.get("check").is_none(), "null check must be omitted");
        assert_eq!(a["assertion_type"], "artifact_present");
    }
}
