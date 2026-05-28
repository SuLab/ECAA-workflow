//! Remediation suggestion + override types.
//!
//! `RemediationSuggestion` is what the proposer (Opus 4.7 side-call)
//! produces from a `ToolErrorEnvelope`. The UI renders ranked
//! suggestions; the SME clicks "Apply & resume"; the server
//! translates the suggestion to:
//! 1. an `ExecutorOverrides` write at
//!    `runtime/inputs/<task>/overrides.json`, and
//! 2. a dispatch through one of the existing closed-vocabulary
//!    mutation tools (`rerun_task`, `amend_stage_method`,
//!    `set_intake_field`).
//!
//! The enum is closed — adding a variant is a deliberate amendment to
//! the remediation contract. This preserves the "LLM as UX shim, not
//! brain" invariant: the LLM proposes, deterministic server gates
//! and applies.

use crate::ids::{StageId, TaskId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

/// Maximum remediation attempts per task before the proposer is
/// short-circuited and the SME must intervene manually. Keeps a bad
/// proposer from looping the harness.
pub const MAX_REMEDIATION_ATTEMPTS: u32 = 5;

/// One ranked remediation proposed for a `BlockerKind::ToolError`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct RemediationSuggestion {
    /// Stable id (UUID-short) used by the apply endpoint to
    /// dispatch the right action.
    pub id: String,
    /// Kind.
    pub kind: RemediationKind,
    /// LLM-authored prose: why this is likely to fix the observed
    /// error. Rendered verbatim under the suggestion header.
    pub rationale: String,
    /// Confidence.
    pub confidence: SuggestionConfidence,
    /// Names of envelope fields that motivated the suggestion
    /// (`error_class`, `signal`, `peak_memory_mb`,...). Used by the
    /// UI to render evidence chips so the SME can sanity-check.
    pub evidence: Vec<String>,
    /// Tool binding.
    pub tool_binding: ToolBinding,
    /// Approximate cost-delta in USD when applying this remediation
    /// on a remote backend (instance-type bumps, on-demand swaps).
    /// `None` for local + free remediations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub estimated_cost_delta_usd: Option<f64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// SuggestionConfidence discriminant.
pub enum SuggestionConfidence {
    /// Low variant.
    Low,
    /// Medium variant.
    Medium,
    /// High variant.
    High,
}

/// Which deterministic mutation tool the apply endpoint dispatches
/// to. Read by the UI to set the button label and confirmation
/// rendering. Always a member of the existing closed 16-tool
/// vocabulary plus two non-tool paths (operator action, manual-only).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ToolBinding {
    /// `rerun_task` on the failing task with overrides applied.
    RerunTask,
    /// `amend_stage_method` (invalidates downstream slice).
    AmendStageMethod,
    /// `set_intake_field` (resumes affected task afterwards).
    SetIntakeField,
    /// `rerun_task` on a different task (an upstream producer).
    RerunUpstreamTask,
    /// Operator must run a command outside the chat surface
    /// (rebuild AMI, install module). UI shows guidance, no
    /// auto-apply button.
    OperatorAction,
    /// Free-form prose; SME judgment required. No apply path.
    ManualOnly,
}

/// Closed remediation taxonomy. Ten variants, each mapping to one
/// `ToolBinding`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RemediationKind {
    /// Resource bump — covers OOM, wallclock, disk-full, GPU absent.
    /// Concrete levers live in `ResourceTarget`; backend translation
    /// happens at apply time.
    BumpResources {
        /// Target.
        target: ResourceTarget,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        /// Prior.
        prior: Option<ResourceTarget>,
    },

    /// Algorithm / library / statistical-test swap at the same stage.
    /// Bound to `amend_stage_method` — invalidates the downstream
    /// slice via the existing dag::invalidate_forward_slice path.
    SwitchMethod {
        stage_id: StageId,
        from: String,
        /// To.
        to: String,
        /// Switch kind.
        switch_kind: MethodSwitchKind,
    },

    /// Same library, different version. Threaded via
    /// `ExecutorOverrides::library_pins` env vars to the agent.
    PinLibraryVersion {
        /// Library.
        library: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        /// From.
        from: Option<String>,
        /// To.
        to: String,
    },

    /// Knob tweak that doesn't change the method. Examples:
    /// relax/tighten thresholds, disable optional sub-step,
    /// switch parser flag.
    OverrideParameter {
        stage_id: StageId,
        param: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(type = "unknown | null", optional)]
        /// From.
        from: Option<serde_json::Value>,
        #[ts(type = "unknown")]
        /// To.
        to: serde_json::Value,
    },

    /// Input-side fix — wrong reference build, wrong annotation,
    /// wrong file format, cohort subset error. Bound to
    /// `set_intake_field`; affected task reruns on resume.
    SwapInputData {
        /// Field.
        field: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        /// From.
        from: Option<String>,
        /// To.
        to: String,
        /// Swap kind.
        swap_kind: InputSwapKind,
    },

    /// Upstream task produced bad output (zero-variance features,
    /// empty matrix, malformed columns). Reruns the producer with a
    /// nested fix; downstream slice invalidates automatically.
    RerunUpstream {
        /// Producer task id.
        producer_task_id: TaskId,
        /// Nested.
        nested: Box<RemediationKind>,
    },

    /// Backend-specific tweak that isn't a resource bump:
    /// AWS spot off, SLURM partition swap, AWS AZ pinning.
    TweakExecutor {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        /// Disable spot.
        disable_spot: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        /// Partition.
        partition: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        /// Availability zone.
        availability_zone: Option<String>,
    },

    /// Transient infra: single S3 503, network timeout, isolated
    /// spot interrupt. Same overrides, new attempt counter.
    RetryAsIs { reason: String },

    /// Container/AMI/SLURM module set is missing a capability.
    /// No auto-apply — UI shows the operator command + waits for
    /// SME to confirm completion.
    RebuildEnvironment {
        /// Capability.
        capability: String,
        /// Operator command hint.
        operator_command_hint: String,
    },

    /// Escape hatch — proposer couldn't match a typed remediation.
    /// UI renders prose; no apply button.
    ManualReview {
        /// Summary.
        summary: String,
        /// Suggested next steps.
        suggested_next_steps: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// MethodSwitchKind discriminant.
pub enum MethodSwitchKind {
    /// Algorithm variant.
    Algorithm,
    /// Library variant.
    Library,
    /// StatisticalTest variant.
    StatisticalTest,
    /// Solver variant.
    Solver,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// InputSwapKind discriminant.
pub enum InputSwapKind {
    /// Reference variant.
    Reference,
    /// Annotation variant.
    Annotation,
    /// Format variant.
    Format,
    /// CohortSubset variant.
    CohortSubset,
}

/// SME-facing resource target. Distinct from the harness's
/// `ResourceRequirements` so the core crate has no harness/cloud
/// dependencies. The harness translates this to its own type at
/// `apply_overrides` time.
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default, TS, schemars::JsonSchema,
)]
#[ts(export)]
pub struct ResourceTarget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    /// Vcpus.
    pub vcpus: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    /// Memory gb.
    pub memory_gb: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    /// Storage gb.
    pub storage_gb: Option<u32>,
    /// Wallclock cap in seconds. Honored by SLURM (`--time`) and the
    /// local SIGTERM timer; AWS does not enforce wallclock natively.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub wallclock_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    /// Gpu.
    pub gpu: Option<GpuTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
/// GpuTarget data.
pub struct GpuTarget {
    /// Backend-agnostic GPU class (e.g. "nvidia-a10",
    /// "nvidia-a100", "nvidia-t4"). Backends map to their own
    /// instance/partition surface.
    pub kind: String,
    /// Count.
    pub count: u32,
}

/// Per-task overrides written by the apply endpoint and read by the
/// executor at dispatch time. Lives at
/// `runtime/inputs/<task_id>/overrides.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ExecutorOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    /// Resources.
    pub resources: Option<ResourceTarget>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    /// Disable spot.
    pub disable_spot: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    /// Partition.
    pub partition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    /// Availability zone.
    pub availability_zone: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    /// Library pins.
    pub library_pins: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(type = "Record<string, unknown>")]
    /// Stage parameters.
    pub stage_parameters: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    /// Env passthrough.
    pub env_passthrough: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    /// Disable pilot.
    pub disable_pilot: bool,
    /// Number of remediation applies recorded so far. Capped at
    /// [`MAX_REMEDIATION_ATTEMPTS`] by the apply endpoint.
    #[serde(default)]
    pub attempts_consumed: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    /// Last applied suggestion id.
    pub last_applied_suggestion_id: Option<String>,
    /// Audit trail. Surfaced on the BlockerCard so the SME can see
    /// what's been tried and the proposer can avoid recommending
    /// the same fix twice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<AppliedRemediation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
/// AppliedRemediation data.
pub struct AppliedRemediation {
    /// Suggestion id.
    pub suggestion_id: String,
    /// Kind.
    pub kind: RemediationKind,
    /// Applied at.
    pub applied_at: String,
    /// `"sme:<email>"` for SME clicks; `"auto:retry-as-is"` for
    /// proposer-driven automatic retries (none today, reserved).
    pub applied_by: String,
    /// Filled by the next attempt. `NotYetAttempted` is the value at
    /// write-time; the executor flips it on the next blocker.
    #[serde(default = "outcome_not_yet_attempted")]
    pub outcome: RemediationOutcome,
}

fn outcome_not_yet_attempted() -> RemediationOutcome {
    RemediationOutcome::NotYetAttempted
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// RemediationOutcome discriminant.
pub enum RemediationOutcome {
    /// Resolved variant.
    Resolved,
    /// Recurred variant.
    Recurred,
    /// NewError variant.
    NewError,
    /// NotYetAttempted variant.
    NotYetAttempted,
}

impl ExecutorOverrides {
    /// Merge a new applied remediation into the override set. The
    /// `kind` parameter dictates which override fields get set;
    /// the audit-trail entry is always appended.
    ///
    /// Returns `Err` when `attempts_consumed` would exceed
    /// [`MAX_REMEDIATION_ATTEMPTS`] — the apply endpoint surfaces
    /// this as a `429 Too Many Attempts` so the SME knows to
    /// switch tactics (manual review, branch session).
    pub fn merge(
        &mut self,
        suggestion: &RemediationSuggestion,
        applied_at: String,
        applied_by: String,
    ) -> Result<(), RemediationMergeError> {
        if self.attempts_consumed >= MAX_REMEDIATION_ATTEMPTS {
            return Err(RemediationMergeError::AttemptsExhausted);
        }
        self.apply_kind(&suggestion.kind);
        self.attempts_consumed = self.attempts_consumed.saturating_add(1);
        self.last_applied_suggestion_id = Some(suggestion.id.clone());
        self.history.push(AppliedRemediation {
            suggestion_id: suggestion.id.clone(),
            kind: suggestion.kind.clone(),
            applied_at,
            applied_by,
            outcome: RemediationOutcome::NotYetAttempted,
        });
        Ok(())
    }

    fn apply_kind(&mut self, kind: &RemediationKind) {
        match kind {
            RemediationKind::BumpResources { target, .. } => {
                let merged = self.resources.clone().unwrap_or_default();
                self.resources = Some(merge_targets(merged, target.clone()));
            }
            RemediationKind::PinLibraryVersion { library, to, .. } => {
                self.library_pins.insert(library.clone(), to.clone());
            }
            RemediationKind::OverrideParameter { param, to, .. } => {
                self.stage_parameters.insert(param.clone(), to.clone());
            }
            RemediationKind::TweakExecutor {
                disable_spot,
                partition,
                availability_zone,
            } => {
                if *disable_spot {
                    self.disable_spot = true;
                }
                if let Some(p) = partition {
                    self.partition = Some(p.clone());
                }
                if let Some(az) = availability_zone {
                    self.availability_zone = Some(az.clone());
                }
            }
            RemediationKind::RerunUpstream { nested, .. } => self.apply_kind(nested),
            // Method swap, input swap, retry-as-is, rebuild env,
            // manual review don't change the override set — they
            // route through their bound mutation tool. The audit
            // entry still records the action.
            RemediationKind::SwitchMethod { .. }
            | RemediationKind::SwapInputData { .. }
            | RemediationKind::RetryAsIs { .. }
            | RemediationKind::RebuildEnvironment { .. }
            | RemediationKind::ManualReview { .. } => {}
        }
    }

    /// Mark the most recent applied remediation with an outcome.
    /// Called by the executor when the next attempt resolves the
    /// blocker (`Resolved`), hits the same error (`Recurred`), or
    /// hits a different error (`NewError`).
    pub fn record_last_outcome(&mut self, outcome: RemediationOutcome) {
        if let Some(last) = self.history.last_mut() {
            last.outcome = outcome;
        }
    }
}

#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
/// RemediationMergeError discriminant.
pub enum RemediationMergeError {
    #[error("remediation attempts exhausted (cap = {})", MAX_REMEDIATION_ATTEMPTS)]
    /// AttemptsExhausted variant.
    AttemptsExhausted,
}

fn merge_targets(base: ResourceTarget, overlay: ResourceTarget) -> ResourceTarget {
    ResourceTarget {
        vcpus: overlay.vcpus.or(base.vcpus),
        memory_gb: overlay.memory_gb.or(base.memory_gb),
        storage_gb: overlay.storage_gb.or(base.storage_gb),
        wallclock_secs: overlay.wallclock_secs.or(base.wallclock_secs),
        gpu: overlay.gpu.or(base.gpu),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suggestion(id: &str, kind: RemediationKind) -> RemediationSuggestion {
        RemediationSuggestion {
            id: id.into(),
            kind,
            rationale: "because".into(),
            confidence: SuggestionConfidence::Medium,
            evidence: vec!["error_class".into()],
            tool_binding: ToolBinding::RerunTask,
            estimated_cost_delta_usd: None,
        }
    }

    #[test]
    fn all_variants_roundtrip_serde() {
        let kinds = vec![
            RemediationKind::BumpResources {
                target: ResourceTarget {
                    memory_gb: Some(64),
                    ..Default::default()
                },
                prior: Some(ResourceTarget {
                    memory_gb: Some(32),
                    ..Default::default()
                }),
            },
            RemediationKind::SwitchMethod {
                stage_id: "alignment".into(),
                from: "STAR".into(),
                to: "HISAT2".into(),
                switch_kind: MethodSwitchKind::Library,
            },
            RemediationKind::PinLibraryVersion {
                library: "scanpy".into(),
                from: Some("1.10.0".into()),
                to: "1.9.6".into(),
            },
            RemediationKind::OverrideParameter {
                stage_id: "filter".into(),
                param: "min_counts".into(),
                from: Some(serde_json::json!(500)),
                to: serde_json::json!(200),
            },
            RemediationKind::SwapInputData {
                field: "reference_genome".into(),
                from: Some("hg38".into()),
                to: "mm10".into(),
                swap_kind: InputSwapKind::Reference,
            },
            RemediationKind::RerunUpstream {
                producer_task_id: "qc_filter".into(),
                nested: Box::new(RemediationKind::OverrideParameter {
                    stage_id: "qc_filter".into(),
                    param: "min_genes".into(),
                    from: None,
                    to: serde_json::json!(100),
                }),
            },
            RemediationKind::TweakExecutor {
                disable_spot: true,
                partition: Some("gpu".into()),
                availability_zone: None,
            },
            RemediationKind::RetryAsIs {
                reason: "transient S3 503".into(),
            },
            RemediationKind::RebuildEnvironment {
                capability: "r-fgsea".into(),
                operator_command_hint: "conda install -c bioconda r-fgsea".into(),
            },
            RemediationKind::ManualReview {
                summary: "needs SME judgment".into(),
                suggested_next_steps: vec!["check input cohort".into()],
            },
        ];
        assert_eq!(kinds.len(), 10);
        for k in kinds {
            let json = serde_json::to_string(&k).unwrap();
            let back: RemediationKind = serde_json::from_str(&json).unwrap();
            assert_eq!(k, back);
        }
    }

    #[test]
    fn merge_bump_resources_accumulates() {
        let mut ov = ExecutorOverrides::default();
        let s1 = suggestion(
            "a",
            RemediationKind::BumpResources {
                target: ResourceTarget {
                    memory_gb: Some(64),
                    ..Default::default()
                },
                prior: None,
            },
        );
        ov.merge(&s1, "t1".into(), "sme:alan@hueb.org".into())
            .unwrap();
        assert_eq!(ov.resources.as_ref().unwrap().memory_gb, Some(64));
        assert_eq!(ov.attempts_consumed, 1);
        assert_eq!(ov.history.len(), 1);

        let s2 = suggestion(
            "b",
            RemediationKind::BumpResources {
                target: ResourceTarget {
                    vcpus: Some(16),
                    memory_gb: Some(128),
                    ..Default::default()
                },
                prior: None,
            },
        );
        ov.merge(&s2, "t2".into(), "sme:alan@hueb.org".into())
            .unwrap();
        let r = ov.resources.as_ref().unwrap();
        assert_eq!(r.memory_gb, Some(128));
        assert_eq!(r.vcpus, Some(16));
        assert_eq!(ov.attempts_consumed, 2);
    }

    #[test]
    fn merge_caps_attempts() {
        let mut ov = ExecutorOverrides {
            attempts_consumed: MAX_REMEDIATION_ATTEMPTS,
            ..Default::default()
        };
        let s = suggestion(
            "x",
            RemediationKind::RetryAsIs {
                reason: "transient".into(),
            },
        );
        let err = ov.merge(&s, "now".into(), "sme:x".into()).unwrap_err();
        assert_eq!(err, RemediationMergeError::AttemptsExhausted);
    }

    #[test]
    fn merge_pin_library_replaces_existing_pin() {
        let mut ov = ExecutorOverrides::default();
        let s1 = suggestion(
            "a",
            RemediationKind::PinLibraryVersion {
                library: "scanpy".into(),
                from: None,
                to: "1.9.6".into(),
            },
        );
        ov.merge(&s1, "t".into(), "sme".into()).unwrap();
        let s2 = suggestion(
            "b",
            RemediationKind::PinLibraryVersion {
                library: "scanpy".into(),
                from: Some("1.9.6".into()),
                to: "1.9.5".into(),
            },
        );
        ov.merge(&s2, "t".into(), "sme".into()).unwrap();
        assert_eq!(ov.library_pins.get("scanpy").unwrap(), "1.9.5");
    }

    #[test]
    fn merge_tweak_executor_sets_flags() {
        let mut ov = ExecutorOverrides::default();
        let s = suggestion(
            "a",
            RemediationKind::TweakExecutor {
                disable_spot: true,
                partition: Some("highmem".into()),
                availability_zone: None,
            },
        );
        ov.merge(&s, "t".into(), "sme".into()).unwrap();
        assert!(ov.disable_spot);
        assert_eq!(ov.partition.as_deref(), Some("highmem"));
        assert!(ov.availability_zone.is_none());
    }

    #[test]
    fn merge_rerun_upstream_unwraps_to_nested() {
        let mut ov = ExecutorOverrides::default();
        let s = suggestion(
            "a",
            RemediationKind::RerunUpstream {
                producer_task_id: "qc".into(),
                nested: Box::new(RemediationKind::BumpResources {
                    target: ResourceTarget {
                        memory_gb: Some(64),
                        ..Default::default()
                    },
                    prior: None,
                }),
            },
        );
        ov.merge(&s, "t".into(), "sme".into()).unwrap();
        assert_eq!(ov.resources.as_ref().unwrap().memory_gb, Some(64));
    }

    #[test]
    fn record_last_outcome_updates_tail() {
        let mut ov = ExecutorOverrides::default();
        let s = suggestion("a", RemediationKind::RetryAsIs { reason: "x".into() });
        ov.merge(&s, "t".into(), "sme".into()).unwrap();
        ov.record_last_outcome(RemediationOutcome::Resolved);
        assert_eq!(
            ov.history.last().unwrap().outcome,
            RemediationOutcome::Resolved
        );
    }

    #[test]
    fn overrides_default_serialises_empty() {
        let ov = ExecutorOverrides::default();
        let json = serde_json::to_string(&ov).unwrap();
        assert_eq!(json, "{\"attempts_consumed\":0}");
    }

    #[test]
    fn suggestion_roundtrip() {
        let s = suggestion(
            "rs-001",
            RemediationKind::BumpResources {
                target: ResourceTarget {
                    memory_gb: Some(64),
                    ..Default::default()
                },
                prior: None,
            },
        );
        let json = serde_json::to_string(&s).unwrap();
        let back: RemediationSuggestion = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }
}
