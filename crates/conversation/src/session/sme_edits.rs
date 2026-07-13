//! SME-authored per-task edit helpers on [`Session`].
//!
//! These are *SME* actions (deterministic REST, never LLM inference): binding
//! concrete parameter values to an atom's declared `ParameterSpec` and authoring
//! per-stage validation bounds. Both the standalone REST endpoints
//! (`set_task_parameters_from_rest` / `set_validation_bound_from_rest`) and the
//! branch-to-edit path (`apply_branch_edits_from_rest`) share this logic so the
//! validation, provenance, and forward-slice invalidation stay DRY.
//!
//! The helpers here deliberately do NOT touch session state, confirmation
//! tokens, or deferred triggers — that lifecycle is owned by the calling
//! transition (a standalone edit clears the confirmation and drains to
//! `ReadyToEmit`; a branch edit is applied to a freshly-branched child before
//! its auto-emit and must preserve the branch's minted token).

use super::Session;
use ecaa_workflow_core::atom::ParameterSpec;
use ecaa_workflow_core::decision_log::{DecisionActor, DecisionType};
use ecaa_workflow_core::parameter_override::{OverrideSource, ParamOverrideError};
use ecaa_workflow_core::validation_bound::{is_supported_assertion_type, SmeValidationBound};
use std::collections::BTreeMap;
use std::path::Path;

/// Failures when applying an SME edit. Not wire-facing (surfaces only as an
/// error string the REST handlers render as 400), so no `#[non_exhaustive]`.
#[derive(Debug, thiserror::Error)]
pub enum SmeEditError {
    /// The named task is not present in the session's DAG.
    #[error("task `{0}` is not present in the session DAG")]
    UnknownTask(String),
    /// A parameter value failed validation against the atom's `ParameterSpec`.
    #[error("{0}")]
    Parameter(#[from] ParamOverrideError),
    /// The assertion type is not one the harness `run_assertion` implements.
    #[error(
        "assertion_type `{0}` is not enforceable by the harness — pick one of the supported types"
    )]
    UnsupportedAssertionType(String),
    /// A remove-by-id request named a bound that does not exist for the stage.
    #[error("validation bound `{bound_id}` not found for stage `{stage_class}`")]
    BoundNotFound {
        /// The bound id the remove targeted.
        bound_id: String,
        /// The stage class the bound was expected under.
        stage_class: String,
    },
    /// The DAG rebuild after invalidating the forward slice failed.
    #[error("forward-slice rebuild failed: {0}")]
    Rebuild(String),
}

impl Session {
    /// True when `task_id` is a member of the current (derived) DAG.
    pub fn dag_contains_task(&self, task_id: &str) -> bool {
        self.current_dag()
            .map(|d| d.tasks.contains_key(task_id))
            .unwrap_or(false)
    }

    /// The `source_atom_id` recorded on a DAG task, if any.
    pub fn task_source_atom_id(&self, task_id: &str) -> Option<String> {
        self.current_dag()?.tasks.get(task_id)?.source_atom_id.clone()
    }

    /// Resolve the atom's declared `ParameterSpec[]` for `task_id` by looking
    /// up the DAG task's `source_atom_id` in the on-disk atom registry
    /// (`<config_dir>/stage-atoms`). Empty when the task, its atom, or the
    /// atom's parameter block cannot be resolved — mirrors the emit-time
    /// resolution in `crates/core/src/emitter/mod.rs`.
    pub fn atom_parameter_specs(&self, task_id: &str, config_dir: &Path) -> Vec<ParameterSpec> {
        let Some(atom_id) = self.task_source_atom_id(task_id) else {
            return Vec::new();
        };
        let atoms_dir = config_dir.join("stage-atoms");
        match ecaa_workflow_core::atom_registry::AtomRegistry::load_cached(&atoms_dir) {
            Ok(reg) => reg
                .get(&atom_id)
                .map(|a| a.parameters.clone())
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    /// The current SME parameter overrides for a task as a flat `name -> value`
    /// map (empty when none). Used by the GET parameters endpoint.
    pub fn current_parameter_overrides(
        &self,
        task_id: &str,
    ) -> BTreeMap<String, serde_json::Value> {
        self.sme_parameter_overrides
            .for_task(task_id)
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.value.clone())).collect())
            .unwrap_or_default()
    }

    /// Apply SME parameter overrides to `task_id`.
    ///
    /// Validates every value against the atom's `ParameterSpec` (fail-closed —
    /// the whole request is rejected on the first error, with NO partial state
    /// committed), sets them into `sme_parameter_overrides`, records one
    /// `SetTaskParameter` decision per changed key, and invalidates + rebuilds
    /// the task's forward slice (downstream results are stale once a value
    /// changes). Returns the invalidated task ids.
    ///
    /// Does NOT change session state, clear confirmation/execution tokens, or
    /// queue deferred triggers — the caller owns that lifecycle.
    pub fn apply_parameter_overrides(
        &mut self,
        task_id: &str,
        overrides: &BTreeMap<String, serde_json::Value>,
        config_dir: &Path,
        actor: DecisionActor,
    ) -> Result<Vec<String>, SmeEditError> {
        if !self.dag_contains_task(task_id) {
            return Err(SmeEditError::UnknownTask(task_id.to_string()));
        }
        if overrides.is_empty() {
            return Ok(Vec::new());
        }
        let specs = self.atom_parameter_specs(task_id, config_dir);

        // Validate on a scratch clone so nothing is committed on error.
        let mut scratch = self.sme_parameter_overrides.clone();
        for (name, value) in overrides {
            scratch.set(task_id, name, value.clone(), OverrideSource::Sme);
        }
        scratch.validate_against(task_id, &specs)?;
        self.sme_parameter_overrides = scratch;

        // Record one decision per key (BTreeMap iteration is deterministic).
        for (name, value) in overrides {
            self.record_decision(
                DecisionType::SetTaskParameter {
                    task_id: task_id.to_string(),
                    parameter: name.clone(),
                    value: value.clone(),
                },
                actor.clone(),
                None,
            );
        }

        // A changed parameter value stales the task's forward slice; reuse the
        // same invalidation helper the method-amend path uses.
        crate::tools::invalidate_and_rebuild(self, task_id, config_dir)
            .map_err(|e| SmeEditError::Rebuild(format!("{e}")))
    }

    /// Add, replace, or remove one SME-authored validation bound.
    ///
    /// - `bound = Some(b)` adds or (when `b.id` already exists for `b.stage_class`)
    ///   replaces the bound. Its `assertion_type` must be harness-runnable.
    /// - `bound = None` removes the bound identified by `(stage_class, bound_id)`;
    ///   an unknown id is an error.
    ///
    /// Records a `SetValidationBound` decision. Does NOT invalidate the DAG —
    /// bounds are post-hoc `result.json` checks — nor change session state.
    pub fn apply_validation_bound(
        &mut self,
        stage_class: &str,
        bound: Option<SmeValidationBound>,
        bound_id: &str,
        actor: DecisionActor,
        rationale: Option<String>,
    ) -> Result<(), SmeEditError> {
        match bound {
            Some(b) => {
                if !is_supported_assertion_type(&b.assertion_type) {
                    return Err(SmeEditError::UnsupportedAssertionType(b.assertion_type));
                }
                let (recorded_stage, recorded_id) = (b.stage_class.clone(), b.id.clone());
                let slot = self
                    .sme_validation_bounds
                    .0
                    .iter_mut()
                    .find(|x| x.id == b.id && x.stage_class == b.stage_class);
                match slot {
                    Some(existing) => *existing = b,
                    None => self.sme_validation_bounds.0.push(b),
                }
                self.record_decision(
                    DecisionType::SetValidationBound {
                        stage_class: recorded_stage,
                        bound_id: recorded_id,
                        removed: false,
                    },
                    actor,
                    rationale,
                );
            }
            None => {
                let before = self.sme_validation_bounds.0.len();
                self.sme_validation_bounds
                    .0
                    .retain(|x| !(x.id == bound_id && x.stage_class == stage_class));
                if self.sme_validation_bounds.0.len() == before {
                    return Err(SmeEditError::BoundNotFound {
                        bound_id: bound_id.to_string(),
                        stage_class: stage_class.to_string(),
                    });
                }
                self.record_decision(
                    DecisionType::SetValidationBound {
                        stage_class: stage_class.to_string(),
                        bound_id: bound_id.to_string(),
                        removed: true,
                    },
                    actor,
                    rationale,
                );
            }
        }
        Ok(())
    }

    /// Apply staged branch edits to THIS (freshly-branched) session: an optional
    /// method change on `task_id`, concrete parameter overrides on `task_id`, and
    /// a set of validation bounds. Records the corresponding `DecisionType`s on
    /// the child. Preserves the branch's confirmation token / state so the
    /// caller's auto-emit still fires (branch edits are applied before emit).
    ///
    /// Returns the task ids invalidated by the parameter/method change.
    pub fn apply_branch_edits(
        &mut self,
        task_id: Option<&str>,
        method: Option<&str>,
        parameters: &BTreeMap<String, serde_json::Value>,
        validation_bounds: &[SmeValidationBound],
        config_dir: &Path,
    ) -> Result<Vec<String>, SmeEditError> {
        let mut invalidated = Vec::new();
        if let Some(tid) = task_id {
            if !self.dag_contains_task(tid) {
                return Err(SmeEditError::UnknownTask(tid.to_string()));
            }
            let method_trimmed = method.map(str::trim).filter(|m| !m.is_empty());
            if let Some(m) = method_trimmed {
                // Method-neutrality holds: this is an SME choice on the child.
                self.intake_methods.set(tid, Some(m.to_string()), None);
                self.record_decision(
                    DecisionType::AmendStage {
                        stage: tid.to_string(),
                        method_prose: m.to_string(),
                    },
                    DecisionActor::Sme,
                    None,
                );
            }
            if !parameters.is_empty() {
                // apply_parameter_overrides folds in the value edits AND rebuilds
                // the forward slice (which also re-derives the DAG so a method
                // change above takes effect).
                invalidated =
                    self.apply_parameter_overrides(tid, parameters, config_dir, DecisionActor::Sme)?;
            } else if method_trimmed.is_some() {
                // Method changed but no parameter edits: still rebuild so the
                // new method reaches the child's emitted DAG.
                invalidated = crate::tools::invalidate_and_rebuild(self, tid, config_dir)
                    .map_err(|e| SmeEditError::Rebuild(format!("{e}")))?;
            }
        }
        for b in validation_bounds {
            let (stage_class, bound_id) = (b.stage_class.clone(), b.id.clone());
            self.apply_validation_bound(
                &stage_class,
                Some(b.clone()),
                &bound_id,
                DecisionActor::Sme,
                None,
            )?;
        }
        Ok(invalidated)
    }
}
