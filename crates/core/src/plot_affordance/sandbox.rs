//! Static sandbox check entry point for drafted renderer modules.
//!
//! Static-analysis gate that wraps
//! `sandbox_policy::check_generated_code_node`. The runtime sandbox
//! execution (deny_network / deny_secrets / host_fs isolation) is deferred.
//!
//! The drafter side-call produces a `DraftedRenderer`; this module
//! constructs a synthetic `TaskNode` with
//! `Implementation::GeneratedCode { review_status: Drafted }` and runs
//! `check_generated_code_node` over it. The outcome
//! is `SandboxOutcome::StaticChecksPassed` (ready for promotion gate) or
//! `SandboxOutcome::Refused { refusals }` (must surface to SME).
//!
//! Because `ReviewStatus` only has `Unreviewed / AutoReviewed /
//! HumanReviewed / Rejected` (the `Drafted` label is a
//! `GeneratedReviewStatus` in the affordance IR, not a `ReviewStatus` in
//! the workflow contracts), a freshly-drafted module maps to
//! `ReviewStatus::Unreviewed` at the `check_generated_code_node` layer.
//! After the static check passes the promotion gate upgrades it to
//! `AutoReviewed`; after SME signoff it becomes `HumanReviewed`.

use crate::sandbox_policy::{check_generated_code_node, SandboxPolicy, SandboxRefusal};
use crate::workflow_contracts::implementation::{Implementation, ReviewStatus};
use crate::workflow_contracts::task_node::TaskNode;

/// Outcome of the static sandbox check for a drafted renderer module.
#[derive(Debug)]
pub enum SandboxOutcome {
    /// All static checks passed for the node. The `task_node_id` is the
    /// synthetic id used in the check — callers use it for audit-log
    /// correlation rather than for dispatch (the actual TaskNode is
    /// constructed at promotion time).
    StaticChecksPassed {
        /// Synthetic task node id used for audit-log correlation.
        task_node_id: String,
    },
    /// One or more static checks refused the module. The SME must review
    /// the refusals before the module can be promoted.
    Refused {
        /// List of refusal details from the static checks.
        refusals: Vec<SandboxRefusal>,
    },
}

/// Run `check_generated_code_node` against a synthetic `TaskNode`
/// constructed from the drafted module source.
///
/// # Arguments
///
/// - `drafted_module_source` — the Python module source from the drafter.
///   Not inspected here (AST analysis is deferred); presence is required
///   so the API reflects the full contract.
/// - `expected_figure_ids` — figure ids from the `DraftedRenderer`. Not
///   inspected here; reserved for a future postcondition check.
/// - `target_stage_id` — the stage id derived from the proposal's
///   `target_semantic_type`. Used as the synthetic `TaskNode` id and as
///   the `repository_ref` fragment so the policy check has a stable id.
/// - `policy` — the `SandboxPolicy` to check against. Pass
///   `SandboxPolicy::default_strict()` for the standard gate.
///
/// # Deferred items
///
/// - AST import-allowlist check (forbid non-allowlisted imports).
/// - `env.read()` / `time()` / un-seeded random call detection.
/// - Actual container sandboxing (deny_network, deny_secrets,
///   deny_host_fs).
pub fn check_drafted_renderer(
    drafted_module_source: &str,
    expected_figure_ids: &[String],
    target_stage_id: &str,
    policy: &SandboxPolicy,
) -> SandboxOutcome {
    // Suppress unused-parameter warnings for the deferred items. When
    // static analysis lands these will be consumed by the AST checker.
    let _ = drafted_module_source;
    let _ = expected_figure_ids;

    // Synthesize a minimal TaskNode representing the drafted module.
    // `review_status: Unreviewed` maps to the freshly-drafted state;
    // the promotion gate advances it to `AutoReviewed` after this check
    // passes and `HumanReviewed` after SME signoff.
    let synthetic_id = format!("generated_renderer_{}", target_stage_id);
    let mut node = TaskNode::skeleton(synthetic_id.clone(), "Generated renderer");
    node.implementation = Implementation::GeneratedCode {
        // The repository_ref is a placeholder path; the harness writes
        // the actual module to `lib/plotting/stages/_generated/<id>.py`
        // at promotion time.
        repository_ref: format!("lib/plotting/stages/_generated/{}.py", target_stage_id),
        review_status: ReviewStatus::Unreviewed,
        artifact_digest: None,
    };

    let refusals = check_generated_code_node(&node, policy);
    if refusals.is_empty() {
        SandboxOutcome::StaticChecksPassed {
            task_node_id: synthetic_id,
        }
    } else {
        SandboxOutcome::Refused { refusals }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_check_passes_for_standard_policy_when_human_review_not_required() {
        // A policy with `require_human_review_for_high_risk: false` and
        // `require_static_analysis: false` passes an Unreviewed node.
        let mut policy = SandboxPolicy::default_strict();
        policy.require_static_analysis = false;
        policy.require_human_review_for_high_risk = false;

        let outcome = check_drafted_renderer(
            "import numpy as np\n",
            &["volcano".to_string()],
            "custom_volcano",
            &policy,
        );
        assert!(
            matches!(outcome, SandboxOutcome::StaticChecksPassed { .. }),
            "expected StaticChecksPassed"
        );
    }

    #[test]
    fn static_check_refused_when_static_analysis_required() {
        // Default strict policy requires static analysis; Unreviewed
        // node triggers StaticAnalysisRequired refusal.
        let policy = SandboxPolicy::default_strict();

        let outcome = check_drafted_renderer(
            "import numpy as np\n",
            &["volcano".to_string()],
            "custom_volcano",
            &policy,
        );
        match outcome {
            SandboxOutcome::Refused { refusals } => {
                assert!(
                    refusals.contains(&SandboxRefusal::StaticAnalysisRequired),
                    "expected StaticAnalysisRequired in {:?}",
                    refusals
                );
            }
            SandboxOutcome::StaticChecksPassed { .. } => {
                panic!("expected Refused under default_strict policy")
            }
        }
    }

    #[test]
    fn sandbox_outcome_refused_variant_tag() {
        let outcome = SandboxOutcome::Refused {
            refusals: vec![SandboxRefusal::StaticAnalysisRequired],
        };
        // Just assert the variant is accessible; pattern matching is the
        // canonical usage.
        assert!(matches!(outcome, SandboxOutcome::Refused { .. }));
    }

    #[test]
    fn synthetic_task_node_id_embeds_stage_id() {
        let mut policy = SandboxPolicy::default_strict();
        policy.require_static_analysis = false;
        policy.require_human_review_for_high_risk = false;

        let outcome = check_drafted_renderer("", &[], "my_volcano_stage", &policy);
        if let SandboxOutcome::StaticChecksPassed { task_node_id } = outcome {
            assert!(
                task_node_id.contains("my_volcano_stage"),
                "task_node_id missing stage_id: {}",
                task_node_id
            );
        }
    }
}
