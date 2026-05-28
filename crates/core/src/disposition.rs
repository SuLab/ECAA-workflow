//! Agent-authored SME disposition: a deferred mutation request the
//! `results_review` (or any other) agent writes when it has enough
//! context to propose a batch of amends / reruns but doesn't have
//! authority to execute them without SME confirmation.
//!
//! A disposition file lives at
//! `runtime/outputs/<task_id>/sme_disposition.json` inside an emitted
//! package. The server's `chat_routes::dispositions` module reads it
//! via the normalizer in this file, renders a `DispositionReviewCard`
//! to the SME, and (on Apply) serially calls the existing
//! `amend_stage_method_from_rest` / `rerun_task_from_rest` endpoints.
//!
//! The normalizer accepts both the v1 schema (`schema_version: 1` +
//! explicit `actions[]`) and a legacy v0 shape
//! (`downstream_invalidation_request.method_amendment` +
//! `stages_to_invalidate_in_order` + `preserved_sme_pins_for_downstream_rerun`)
//! and converts each into a common `Disposition { actions: Vec<Action> }`
//! representation.
//!
//! Round-trip is deliberately lossy: the canonical form is the v1
//! shape. The legacy v0 fields pass through as-is (written back
//! unchanged when the server rewrites the file with `status: "applied"`)
//! for audit-trail fidelity, but `actions[]` is the source of truth
//! for the apply loop.

use crate::ids::TaskId;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Current on-disk schema version written by new agents / consumed
/// canonically by the normalizer. v0 disposition files are accepted and
/// normalised; their `schema_version` field is absent or 0 and the
/// server rewrites them to v1 on first read.
pub const DISPOSITION_SCHEMA_VERSION: u32 = 1;

/// Closed vocabulary of actions a disposition can request. Adding a
/// new variant is a plan-level change — the apply loop in
/// `crates/server/src/chat_routes/dispositions.rs` must grow a match
/// arm, and UI copy in `DispositionReviewCard.tsx` must know how to
/// render it.
///
/// `kind` is internally tagged (`#[serde(tag = "kind", rename_all =
/// "snake_case")]`) so on-wire JSON is flat — e.g.
/// `{"kind":"amend_method","target_stage":"...", "new_method":"..."}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Replace the method for `target_stage` with `new_method`. Maps
    /// to `POST /api/chat/session/:id/task/:target_stage/amend-method`.
    /// The server-side handler already forward-invalidates the DAG
    /// slice, so an immediately-following `InvalidateSlice` is
    /// redundant / advisory.
    AmendMethod {
        /// Target stage.
        target_stage: String,
        /// New method.
        new_method: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        /// Rationale.
        rationale: Option<String>,
        /// Passed through; not consumed by the apply loop itself but
        /// surfaced to the SME in the card preview. Retained so the
        /// agent doesn't lose context when the disposition file is
        /// rewritten with `status: "applied"`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        per_compartment: Option<Vec<String>>,
        /// v1 passthrough for the v0 `preserve_sme_pins` boolean. Not
        /// consumed by the apply loop yet — `preserve_pin` is a v2
        /// action kind (§4.1 of the plan).
        #[serde(default = "default_true")]
        preserve_sme_pins: bool,
    },
    /// Re-run `target_stage` with the method it already has. Maps to
    /// `POST /api/chat/session/:id/task/:target_stage/rerun`. Use for
    /// "the method is right but the output is stale / broken" —
    /// `amend_method` is the right call when the method needs to change.
    Rerun {
        /// Target stage.
        target_stage: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        /// Reason.
        reason: Option<String>,
    },
    /// Advisory: "after the preceding amend/rerun runs, also invalidate
    /// this forward slice." The server's amend handler already does
    /// this automatically via `invalidate_forward_slice`, so this
    /// action is a no-op at apply time — retained in the schema so
    /// `stages_to_invalidate_in_order` from v0 dispositions round-trips
    /// without loss and the card can render it as a preview.
    InvalidateSlice {
        /// From stage.
        from_stage: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        /// Stages explicit.
        stages_explicit: Vec<String>,
        /// Optional passthrough from v0 `preserved_sme_pins_for_downstream_rerun`
        /// so the audit trail survives normalisation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional, type = "unknown | null")]
        preserved_pins: Option<serde_json::Value>,
    },
    /// v2 reservation (§4.1 of the plan): pin a method so the re-run
    /// doesn't re-score it. Accepted + round-tripped but the apply
    /// loop skips it in v1. Present here so v0 `preserve_pin`-style
    /// requests can be normalised without breaking the schema later.
    PreservePin {
        /// Target stage.
        target_stage: String,
        /// Method.
        method: String,
    },
}

fn default_true() -> bool {
    true
}

/// Lifecycle status of a disposition on disk. Written back by the
/// server after apply / reject. `partial` means apply started, one or
/// more actions succeeded, and then one failed — the applied ones are
/// not rolled back (the server never reverts executed mutations) and
/// the card stays expanded with per-action retry affordance.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum DispositionStatus {
    #[default]
    /// Pending variant.
    Pending,
    /// Applied variant.
    Applied,
    /// Rejected variant.
    Rejected,
    /// Partial variant.
    Partial,
}

/// Normalised in-memory shape of `sme_disposition.json`. The normalizer
/// in `from_raw_json` is tolerant of both v0 and v1 inputs.
///
/// Fields not named here pass through `legacy_passthrough` as a free
/// `serde_json::Value` so audit fidelity is preserved across writes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct Disposition {
    /// Written as [`DISPOSITION_SCHEMA_VERSION`] on re-write. Absent
    /// fields in v0 files are normalised to 1 in memory.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// Task id.
    pub task_id: TaskId,
    /// RFC-3339 UTC timestamp when the disposition was authored.
    /// Optional on read (v0 files don't always have one) but always
    /// written back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub created_at: Option<String>,
    /// Free-form "what the agent concluded" prose. Shown at the top of
    /// the `DispositionReviewCard`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub authoritative_interpretation: Option<String>,
    /// Canonical action list. Authored directly in v1 files; derived
    /// from `downstream_invalidation_request` in v0 files.
    #[serde(default)]
    pub actions: Vec<Action>,
    /// Opt-in v2 escape hatch: when both this flag and the
    /// `ECAA_AUTO_APPLY_DISPOSITIONS=1` env var are set, the server
    /// applies the disposition immediately on ingest without waiting
    /// for an SME click. See §5.4 of the plan.
    #[serde(default)]
    pub auto_apply: bool,
    #[serde(default)]
    /// Status.
    pub status: DispositionStatus,
    /// RFC-3339 UTC timestamp of the last status change. Server writes
    /// on apply / reject; never generated by the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub status_updated_at: Option<String>,
    /// Every field the agent wrote that the normalizer didn't claim as
    /// canonical. Round-tripped verbatim so the file-on-disk keeps
    /// semantic fidelity after the server re-writes it on apply /
    /// reject. The field-level serde_json::Value isn't part of the
    /// ts-rs export — the UI never reads it.
    #[serde(flatten, default)]
    #[ts(skip)]
    pub legacy_passthrough: serde_json::Map<String, serde_json::Value>,
}

fn default_schema_version() -> u32 {
    DISPOSITION_SCHEMA_VERSION
}

/// Normalize a raw JSON value (from `serde_json::from_slice(&disk)`)
/// into a canonical [`Disposition`]. Accepts both v1 and v0 shapes
/// transparently. Returns the reason string as `Err` when the file is
/// not even shaped like a disposition (missing `task_id`, not an
/// object, etc.) — callers should log and skip, never 500.
pub fn normalize(raw: serde_json::Value) -> Result<Disposition, String> {
    let mut obj = match raw {
        serde_json::Value::Object(m) => m,
        other => {
            return Err(format!(
                "disposition must be a JSON object, got {:?}",
                value_shape(&other)
            ))
        }
    };

    let task_id = obj
        .get("task_id")
        .and_then(|v| v.as_str())
        .map(TaskId::from)
        .ok_or_else(|| "disposition missing required `task_id` string field".to_string())?;

    let schema_version = obj
        .get("schema_version")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    let created_at = obj
        .get("created_at")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let authoritative_interpretation = obj
        .get("authoritative_interpretation")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let auto_apply = obj
        .get("auto_apply")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let status: DispositionStatus = obj
        .get("status")
        .and_then(|v| v.as_str())
        .map(|s| match s {
            "applied" => DispositionStatus::Applied,
            "rejected" => DispositionStatus::Rejected,
            "partial" => DispositionStatus::Partial,
            _ => DispositionStatus::Pending,
        })
        .unwrap_or_default();

    let status_updated_at = obj
        .get("status_updated_at")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Pick the canonical actions[] array if present (v1). Otherwise
    // derive from the legacy `downstream_invalidation_request` block.
    let mut actions: Vec<Action> = match obj.get("actions") {
        Some(serde_json::Value::Array(arr)) if !arr.is_empty() => {
            arr.iter().filter_map(parse_action).collect()
        }
        _ => derive_actions_from_legacy(obj.get("downstream_invalidation_request")),
    };

    // Filter any `PreservePin` with empty fields — the agent may emit
    // a pin entry without target, which we drop rather than round-trip
    // as broken. Pass-through otherwise.
    actions.retain(|a| match a {
        Action::AmendMethod {
            target_stage,
            new_method,
            ..
        } => !target_stage.is_empty() && !new_method.is_empty(),
        Action::Rerun { target_stage, .. } => !target_stage.is_empty(),
        Action::InvalidateSlice { from_stage, .. } => !from_stage.is_empty(),
        Action::PreservePin {
            target_stage,
            method,
        } => !target_stage.is_empty() && !method.is_empty(),
    });

    // Strip the fields we've already claimed so what's left in
    // `legacy_passthrough` is pure audit passthrough.
    for key in [
        "schema_version",
        "task_id",
        "created_at",
        "authoritative_interpretation",
        "actions",
        "auto_apply",
        "status",
        "status_updated_at",
    ] {
        obj.remove(key);
    }

    Ok(Disposition {
        schema_version: if schema_version == 0 {
            DISPOSITION_SCHEMA_VERSION
        } else {
            schema_version
        },
        task_id,
        created_at,
        authoritative_interpretation,
        actions,
        auto_apply,
        status,
        status_updated_at,
        legacy_passthrough: obj,
    })
}

fn value_shape(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Parse one raw JSON object as an [`Action`]. Returns None when the
/// `kind` is absent or unknown; unknown kinds log a breadcrumb to
/// stderr (matching the blocker-kind mapper idiom) and are dropped
/// from the plan.
fn parse_action(v: &serde_json::Value) -> Option<Action> {
    let obj = v.as_object()?;
    let kind = obj.get("kind").and_then(|k| k.as_str())?;
    match kind {
        "amend_method" => {
            let target_stage = obj
                .get("target_stage")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string();
            let new_method = obj
                .get("new_method")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string();
            let rationale = obj
                .get("rationale")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string());
            let per_compartment =
                obj.get("per_compartment")
                    .and_then(|s| s.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|x| x.as_str().map(|s| s.to_string()))
                            .collect()
                    });
            let preserve_sme_pins = obj
                .get("preserve_sme_pins")
                .and_then(|b| b.as_bool())
                .unwrap_or(true);
            Some(Action::AmendMethod {
                target_stage,
                new_method,
                rationale,
                per_compartment,
                preserve_sme_pins,
            })
        }
        "rerun" => {
            let target_stage = obj
                .get("target_stage")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string();
            let reason = obj
                .get("reason")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string());
            Some(Action::Rerun {
                target_stage,
                reason,
            })
        }
        "invalidate_slice" => {
            let from_stage = obj
                .get("from_stage")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string();
            let stages_explicit = obj
                .get("stages_explicit")
                .and_then(|s| s.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let preserved_pins = obj.get("preserved_pins").cloned();
            Some(Action::InvalidateSlice {
                from_stage,
                stages_explicit,
                preserved_pins,
            })
        }
        "preserve_pin" => {
            let target_stage = obj
                .get("target_stage")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string();
            let method = obj
                .get("method")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string();
            Some(Action::PreservePin {
                target_stage,
                method,
            })
        }
        unknown => {
            eprintln!(
                "[disposition_action_unknown] kind={:?} — dropped from action plan",
                unknown
            );
            None
        }
    }
}

/// Convert the v0 `downstream_invalidation_request` block into the
/// canonical action list. The v0 shape:
///
/// ```json
/// {
/// "stages_to_invalidate_in_order": ["batch_correction",...],
/// "method_amendment": {
/// "stage": "batch_correction",
/// "chosen_method": "cca_integratelayers",
/// "per_compartment": ["NP","AF","CEP"],
///...
/// },
/// "preserved_sme_pins_for_downstream_rerun": {... }
/// }
/// ```
///
/// Maps to `[AmendMethod {... }, InvalidateSlice {... }]`.
fn derive_actions_from_legacy(req: Option<&serde_json::Value>) -> Vec<Action> {
    let Some(req) = req else {
        return Vec::new();
    };
    let Some(obj) = req.as_object() else {
        return Vec::new();
    };
    let mut out = Vec::new();

    if let Some(amend) = obj.get("method_amendment").and_then(|v| v.as_object()) {
        let target_stage = amend
            .get("stage")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let new_method = amend
            .get("chosen_method")
            .or_else(|| amend.get("method"))
            .or_else(|| amend.get("new_method"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if !target_stage.is_empty() && !new_method.is_empty() {
            let rationale = amend
                .get("rationale")
                .or_else(|| amend.get("activation_override"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let per_compartment =
                amend
                    .get("per_compartment")
                    .and_then(|s| s.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|x| x.as_str().map(|s| s.to_string()))
                            .collect()
                    });
            out.push(Action::AmendMethod {
                target_stage: target_stage.clone(),
                new_method,
                rationale,
                per_compartment,
                preserve_sme_pins: true,
            });
            // Treat the first stage in `stages_to_invalidate_in_order`
            // as the anchor of the invalidation slice. We include it
            // as an explicit InvalidateSlice action (advisory) so the
            // card can render the 12-stage fan-out even though the
            // amend already invalidates them server-side.
            let stages: Vec<String> = obj
                .get("stages_to_invalidate_in_order")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let preserved_pins = obj.get("preserved_sme_pins_for_downstream_rerun").cloned();
            let from_stage = stages.first().cloned().unwrap_or(target_stage);
            out.push(Action::InvalidateSlice {
                from_stage,
                stages_explicit: stages,
                preserved_pins,
            });
        }
    }

    out
}

/// Convenience constructor for tests / fixture authoring. Produces a
/// disposition whose `actions[]` is the canonical source of truth.
#[cfg(test)]
pub fn disposition_for_tests(task_id: &str, actions: Vec<Action>) -> Disposition {
    Disposition {
        schema_version: DISPOSITION_SCHEMA_VERSION,
        task_id: TaskId::from(task_id),
        created_at: Some("2026-04-24T00:09:56Z".into()),
        authoritative_interpretation: None,
        actions,
        auto_apply: false,
        status: DispositionStatus::Pending,
        status_updated_at: None,
        legacy_passthrough: serde_json::Map::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v0 shape regression: single batch_correction amend + 12-stage
    /// invalidate slice.
    #[test]
    fn normalize_v0_ivd_disposition_yields_amend_plus_invalidate() {
        let raw = serde_json::json!({
            "task_id": "results_review",
            "authoritative_interpretation": "IVD signal is real but batch structure dominates PC1.",
            "downstream_invalidation_request": {
                "stages_to_invalidate_in_order": [
                    "batch_correction",
                    "integration",
                    "dimensionality_reduction",
                    "clustering",
                    "cell_type_annotation",
                    "differential_expression",
                    "pathway_enrichment",
                    "cell_cell_communication",
                    "trajectory_analysis",
                    "biological_interpretation",
                    "claim_boundary",
                    "results_review"
                ],
                "method_amendment": {
                    "stage": "batch_correction",
                    "chosen_method": "cca_integratelayers",
                    "per_compartment": ["NP", "AF", "CEP"],
                    "activation_override": "SME-approved override based on PC1 residual batch signal"
                },
                "preserved_sme_pins_for_downstream_rerun": {
                    "annotation": "sctype_fenesys"
                }
            }
        });
        let d = normalize(raw).expect("normalize succeeds");
        assert_eq!(d.task_id.as_str(), "results_review");
        assert_eq!(d.schema_version, DISPOSITION_SCHEMA_VERSION);
        assert_eq!(d.actions.len(), 2, "expected amend + invalidate");
        match &d.actions[0] {
            Action::AmendMethod {
                target_stage,
                new_method,
                per_compartment,
                rationale,
                preserve_sme_pins,
            } => {
                assert_eq!(target_stage, "batch_correction");
                assert_eq!(new_method, "cca_integratelayers");
                assert_eq!(
                    per_compartment.as_deref(),
                    Some(vec!["NP".into(), "AF".into(), "CEP".into()].as_slice())
                );
                assert!(
                    rationale
                        .as_deref()
                        .map(|s| s.contains("PC1"))
                        .unwrap_or(false),
                    "rationale should include activation_override prose"
                );
                assert!(preserve_sme_pins);
            }
            other => panic!("expected AmendMethod, got {:?}", other),
        }
        match &d.actions[1] {
            Action::InvalidateSlice {
                from_stage,
                stages_explicit,
                preserved_pins,
            } => {
                assert_eq!(from_stage, "batch_correction");
                assert_eq!(stages_explicit.len(), 12);
                assert_eq!(stages_explicit[0], "batch_correction");
                assert_eq!(stages_explicit[11], "results_review");
                assert!(preserved_pins.is_some());
            }
            other => panic!("expected InvalidateSlice, got {:?}", other),
        }
        assert_eq!(d.status, DispositionStatus::Pending);
    }

    /// v1 native shape: actions[] already present, normalizer passes
    /// through.
    #[test]
    fn normalize_v1_native_passes_actions_through() {
        let raw = serde_json::json!({
            "schema_version": 1,
            "task_id": "results_review",
            "created_at": "2026-04-24T00:09:56Z",
            "authoritative_interpretation": "…",
            "actions": [
                {
                    "kind": "amend_method",
                    "target_stage": "batch_correction",
                    "new_method": "cca_integratelayers",
                    "rationale": "PC1 residual",
                    "per_compartment": ["NP", "AF", "CEP"],
                    "preserve_sme_pins": true
                },
                {
                    "kind": "rerun",
                    "target_stage": "results_review",
                    "reason": "re-score against invalidation slice"
                },
                {
                    "kind": "preserve_pin",
                    "target_stage": "annotation",
                    "method": "sctype_fenesys"
                }
            ]
        });
        let d = normalize(raw).expect("normalize succeeds");
        assert_eq!(d.actions.len(), 3);
        assert!(matches!(d.actions[0], Action::AmendMethod { .. }));
        assert!(matches!(d.actions[1], Action::Rerun { .. }));
        assert!(matches!(d.actions[2], Action::PreservePin { .. }));
    }

    /// Unknown `kind` is dropped and logged. Rest of the plan survives.
    #[test]
    fn normalize_drops_unknown_action_kinds() {
        let raw = serde_json::json!({
            "task_id": "results_review",
            "actions": [
                { "kind": "amend_method", "target_stage": "s1", "new_method": "m1" },
                { "kind": "future_kind_the_agent_invented", "foo": "bar" },
                { "kind": "rerun", "target_stage": "s2" }
            ]
        });
        let d = normalize(raw).expect("normalize succeeds");
        assert_eq!(d.actions.len(), 2);
        assert!(
            matches!(&d.actions[0], Action::AmendMethod { target_stage, .. } if target_stage == "s1")
        );
        assert!(
            matches!(&d.actions[1], Action::Rerun { target_stage, .. } if target_stage == "s2")
        );
    }

    #[test]
    fn normalize_rejects_non_object_root() {
        let raw = serde_json::json!([1, 2, 3]);
        assert!(normalize(raw).is_err());
    }

    #[test]
    fn normalize_rejects_missing_task_id() {
        let raw = serde_json::json!({
            "authoritative_interpretation": "…"
        });
        assert!(normalize(raw).is_err());
    }

    #[test]
    fn normalize_preserves_legacy_passthrough_fields() {
        // Fields the normalizer doesn't know about survive round-trip
        // so the audit trail on disk isn't lossy.
        let raw = serde_json::json!({
            "task_id": "results_review",
            "actions": [],
            "env_capability_preflight": { "seurat": "5.0.3" },
            "sme_exclusions_preserved": ["sample-47"]
        });
        let d = normalize(raw).expect("normalize succeeds");
        assert!(d
            .legacy_passthrough
            .contains_key("env_capability_preflight"));
        assert!(d
            .legacy_passthrough
            .contains_key("sme_exclusions_preserved"));
        assert!(!d.legacy_passthrough.contains_key("task_id"));
    }

    /// v1 canonical round-trip: serialize → deserialize yields the
    /// same struct. Guards the ts-rs export from silent drift.
    #[test]
    fn disposition_v1_roundtrip_serde() {
        let d = disposition_for_tests(
            "results_review",
            vec![
                Action::AmendMethod {
                    target_stage: "batch_correction".into(),
                    new_method: "cca_integratelayers".into(),
                    rationale: Some("batch residual".into()),
                    per_compartment: Some(vec!["NP".into()]),
                    preserve_sme_pins: true,
                },
                Action::InvalidateSlice {
                    from_stage: "batch_correction".into(),
                    stages_explicit: vec!["integration".into()],
                    preserved_pins: Some(serde_json::json!({"annotation": "sctype"})),
                },
            ],
        );
        let json = serde_json::to_string(&d).expect("serialize");
        let back: Disposition = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(d, back);
    }

    /// Status serialization shape is flat snake-case scalars — the UI
    /// treats it as a bare string.
    #[test]
    fn disposition_status_is_flat_scalar() {
        assert_eq!(
            serde_json::to_string(&DispositionStatus::Pending).unwrap(),
            "\"pending\""
        );
        assert_eq!(
            serde_json::to_string(&DispositionStatus::Applied).unwrap(),
            "\"applied\""
        );
        assert_eq!(
            serde_json::to_string(&DispositionStatus::Rejected).unwrap(),
            "\"rejected\""
        );
        assert_eq!(
            serde_json::to_string(&DispositionStatus::Partial).unwrap(),
            "\"partial\""
        );
    }

    /// An empty action list + a legacy block produces a usable plan.
    #[test]
    fn normalize_prefers_actions_over_legacy_when_both_present() {
        let raw = serde_json::json!({
            "task_id": "results_review",
            "actions": [
                { "kind": "rerun", "target_stage": "s_from_v1" }
            ],
            "downstream_invalidation_request": {
                "method_amendment": {
                    "stage": "s_from_v0",
                    "chosen_method": "m0"
                }
            }
        });
        let d = normalize(raw).unwrap();
        assert_eq!(d.actions.len(), 1);
        match &d.actions[0] {
            Action::Rerun { target_stage, .. } => assert_eq!(target_stage, "s_from_v1"),
            other => panic!("expected Rerun, got {:?}", other),
        }
    }

    /// Empty `actions: []` + no legacy block yields an empty plan,
    /// not an error. The card renders a "no actions proposed" banner.
    #[test]
    fn normalize_accepts_empty_action_list() {
        let raw = serde_json::json!({
            "task_id": "results_review",
            "actions": []
        });
        let d = normalize(raw).unwrap();
        assert!(d.actions.is_empty());
    }

    #[test]
    fn normalize_applies_default_schema_version_to_v0_files() {
        let raw = serde_json::json!({
            "task_id": "results_review",
            "actions": []
        });
        let d = normalize(raw).unwrap();
        assert_eq!(d.schema_version, DISPOSITION_SCHEMA_VERSION);
    }
}
