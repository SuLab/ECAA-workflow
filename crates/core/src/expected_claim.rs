//! `ExpectedClaimManifest` — the recall anchor. Derived deterministically
//! at emit time from the intake goal + the DAG's confirmatory-stage
//! outputs. Reconciled against the agent's structured `result.json
//! claims[]` into a `CoverageResult` (see `coverage.rs`) that the
//! audit-proof `claim_completeness` invariant reads. Pure over intake
//! bytes: `BTreeMap`-keyed, no `Clock`, no RNG, no filesystem.

use crate::classify::ClassificationResult;
use crate::dag::{TaskKind, DAG};
use crate::project_class::ProjectClass;
use serde::{Deserialize, Serialize};
use std::path::Path;
use ts_rs::TS;

/// Whether a manifest entry must be addressed for the package to pass
/// the recall floor. `Required` entries that are absent or unverifiable
/// block; `Optional` entries are advisory.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    TS,
    schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Requirement {
    /// Must be addressed by a `Verified` structured claim.
    Required,
    /// Advisory — absence does not block.
    Optional,
}

/// One expected claim the package should produce a verified result for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ExpectedClaim {
    /// The expected entity / subject (gene symbol, endpoint code, or a
    /// stage-level subject token when no specific entity is named).
    pub entity: String,
    /// The contrast / comparison the claim is about, when the intake
    /// names one (e.g. "treated_vs_control"). `None` for cohort- or
    /// output-level expectations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub contrast: Option<String>,
    /// The result table the confirmatory stage is expected to write,
    /// used by the verifier to disambiguate twin candidate tables.
    /// `None` when the stage's output table cannot be named at emit time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub expected_output_table: Option<String>,
    /// Required vs Optional.
    pub requirement: Requirement,
    /// EDAM data IRI typing the expected output, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub edam_data: Option<String>,
}

/// Deterministic manifest of expected claims for a package. Serialized
/// into `policies/interpretation-policy.json` under
/// `verifiableEntities.expected` (an additive promotion of the existing
/// block, not a new subgraph).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ExpectedClaimManifest {
    /// On-disk shape version of the `expected` block.
    pub schema_version: String,
    /// Expected entries, deterministically ordered.
    pub entries: Vec<ExpectedClaim>,
}

/// The set of confirmatory-stage id stems whose output tables anchor the
/// recall floor. These are the stages whose numeric results the verifier
/// recomputes from source; a `Required` expectation is emitted for each
/// present in the DAG. Kept as a small curated list (not a free-for-all
/// over every Computation task) so the manifest under-generates rather
/// than over-generates `Required` entries on novel archetypes.
const CONFIRMATORY_STAGE_STEMS: &[&str] = &[
    "differential_expression",
    "differential_abundance",
    "pathway_enrichment",
    "gene_set_enrichment",
    "variant_calling",
    "peak_calling",
    "primary_endpoint",
    "endpoint_analysis",
];

/// A task is a confirmatory result-producing stage when its source atom
/// id (preferred) or task id stem matches a known confirmatory stage and
/// it is a `Computation` task (not `Discovery`/`Validation`/`Review`/
/// `Gate`). `discover_*` and `validate_*` self-describing stages never
/// anchor expectations.
fn is_confirmatory_stage(task_id: &str, task: &crate::dag::Task) -> bool {
    if !matches!(task.kind, TaskKind::Computation) {
        return false;
    }
    let key = task
        .source_atom_id
        .as_deref()
        .unwrap_or(task_id)
        .to_ascii_lowercase();
    if key.starts_with("discover_") || key.starts_with("validate_") {
        return false;
    }
    CONFIRMATORY_STAGE_STEMS
        .iter()
        .any(|stem| key == *stem || key.contains(stem))
}

/// Derive the expected-claim manifest deterministically from the intake
/// goal + the DAG's confirmatory stages. Pure over intake bytes — no
/// `Clock`, no RNG, no filesystem. `BTreeMap`-keyed accumulation then a
/// final sort by `(requirement, entity, contrast)` so two emits of the
/// same intake produce a byte-identical manifest.
pub fn derive_expected_manifest(
    classification: &ClassificationResult,
    dag: &DAG,
    _project_class: ProjectClass,
) -> ExpectedClaimManifest {
    use std::collections::BTreeMap;

    // Key: (entity, contrast) → ExpectedClaim. BTreeMap dedupes + orders.
    let mut by_key: BTreeMap<(String, Option<String>), ExpectedClaim> = BTreeMap::new();

    // One Required expectation per confirmatory stage present in the DAG.
    // The stage's id stem is the subject + expected output table (the
    // verifier resolves it to the agent's actual table at verify time; a
    // present cited path beats the fuzzy collapse — see `claim_verifier`).
    let goal_edam = classification.goal.as_ref().map(|g| g.edam_data.clone());
    for (task_id, task) in dag.tasks.iter() {
        if !is_confirmatory_stage(task_id.as_ref(), task) {
            continue;
        }
        let stem = task
            .source_atom_id
            .as_deref()
            .unwrap_or(task_id.as_ref())
            .to_ascii_lowercase();
        by_key
            .entry((stem.clone(), None))
            .or_insert_with(|| ExpectedClaim {
                entity: stem.clone(),
                contrast: None,
                expected_output_table: Some(stem.clone()),
                requirement: Requirement::Required,
                edam_data: goal_edam.clone(),
            });
    }

    let mut entries: Vec<ExpectedClaim> = by_key.into_values().collect();
    // Final ordering: Required before Optional, then entity, then contrast.
    entries.sort_by(|a, b| {
        a.requirement
            .cmp(&b.requirement)
            .then_with(|| a.entity.cmp(&b.entity))
            .then_with(|| a.contrast.cmp(&b.contrast))
    });

    ExpectedClaimManifest {
        schema_version: "1".to_string(),
        entries,
    }
}

/// Rel path of the per-package interpretation policy the verifier reads.
const POLICY_REL: &str = "policies/interpretation-policy.json";

/// Inject the derived manifest's `entries` into the package's
/// `policies/interpretation-policy.json` under
/// `verifiableEntities.expected`. Called AFTER `copy_policies` byte-copies
/// the static config policy, so this is the per-package promotion of the
/// shared `verifiableEntities` block. Deterministic over the manifest
/// (and thus over intake): `serde_json::to_vec_pretty` is stable, the
/// manifest is sorted, so two emits of the same intake stay byte-identical
/// — which keeps `policies/` (BagIt-manifested) reproducible.
///
/// No-op (returns Ok) when the policy file is absent — packages emitted
/// from a tree without `config/downstream-policy/` stay byte-identical to
/// the baseline. Writes an empty `expected: []` when the manifest is empty
/// so the verifier can distinguish "no expectations declared" from "block
/// missing entirely".
pub fn inject_manifest_into_policy(
    package_root: &Path,
    manifest: &ExpectedClaimManifest,
) -> anyhow::Result<()> {
    let path = package_root.join(POLICY_REL);
    if !path.exists() {
        return Ok(());
    }
    let raw = std::fs::read_to_string(&path)?;
    let mut policy: serde_json::Value = serde_json::from_str(&raw)?;
    // Only inject when the verifiableEntities block exists (enabled or not);
    // a policy with no such block isn't a claim-verification policy.
    let Some(ve) = policy
        .get_mut("verifiableEntities")
        .and_then(|v| v.as_object_mut())
    else {
        return Ok(());
    };
    ve.insert(
        "expected".to_string(),
        serde_json::to_value(&manifest.entries)?,
    );
    // Atomic write so a torn write never leaves a half-policy. `to_vec_pretty`
    // is deterministic for a fixed Value.
    let bytes = serde_json::to_vec_pretty(&policy)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::{Task, TaskId, TaskState, DAG};
    use std::collections::BTreeMap;

    // `DAG` does NOT derive `Default` (verified: it has `version`,
    // `schema_version`, `workflow_id`, `current_task`, `tasks`,
    // `reverse_deps`, `run_id`). Build a minimal valid DAG here.
    fn dag_with(tasks: Vec<(&str, Task)>) -> DAG {
        let mut dag = DAG {
            version: "1.0".into(),
            schema_version: crate::dag::current_dag_schema_version(),
            workflow_id: "test".into(),
            current_task: None,
            tasks: tasks
                .into_iter()
                .map(|(k, v)| (TaskId::from(k), v))
                .collect(),
            reverse_deps: BTreeMap::new(),
            run_id: None,
        };
        dag.rebuild_reverse_deps();
        dag
    }

    fn comp_task(id: &str) -> Task {
        Task {
            kind: TaskKind::Computation,
            state: TaskState::Pending,
            depends_on: vec![],
            assignee: crate::dag::Assignee::Agent,
            description: id.into(),
            spec: None,
            resolution: None,
            result_ref: None,
            resource_class: Default::default(),
            requires_sme_review: false,
            required_artifacts: vec![],
            container: None,
            source_atom_id: Some(id.into()),
            safety: Default::default(),
        }
    }

    fn empty_classification() -> ClassificationResult {
        // Minimal synthetic classification — no goal, no archetype.
        ClassificationResult {
            modality: "bulk_rnaseq".into(),
            taxonomy_path: String::new(),
            domain: String::new(),
            workflow_description: String::new(),
            edam_topic: String::new(),
            edam_operation: String::new(),
            confidence: 1.0,
            confidence_label: "high".into(),
            organisms: vec![],
            methods_specified: vec![],
            data_sources: vec![],
            intake_text: String::new(),
            goal: None,
            archetype_id: None,
            additional_modalities: vec![],
            tie_candidates: vec![],
        }
    }

    #[test]
    fn empty_dag_and_no_goal_yields_empty_manifest() {
        let dag = dag_with(vec![]);
        let m =
            derive_expected_manifest(&empty_classification(), &dag, ProjectClass::Bioinformatics);
        assert_eq!(m.schema_version, "1");
        assert!(
            m.entries.is_empty(),
            "no confirmatory stages + no goal ⇒ no expectations"
        );
    }

    #[test]
    fn confirmatory_computation_stage_yields_required_entry() {
        // A DAG with one Computation task whose source-atom id names a
        // confirmatory stage (`differential_expression`) yields one
        // Required entry keyed on the stage subject + expected table.
        let dag = dag_with(vec![(
            "differential_expression",
            comp_task("differential_expression"),
        )]);
        let m =
            derive_expected_manifest(&empty_classification(), &dag, ProjectClass::Bioinformatics);
        assert_eq!(m.entries.len(), 1, "one confirmatory stage ⇒ one entry");
        let e = &m.entries[0];
        assert_eq!(e.requirement, Requirement::Required);
        assert_eq!(
            e.expected_output_table.as_deref(),
            Some("differential_expression")
        );
    }

    #[test]
    fn entries_are_deterministically_ordered() {
        // Two confirmatory stages inserted out of sorted order must come
        // back sorted (Required-first, then entity) so two emits of the
        // same intake produce byte-identical manifests.
        let dag = dag_with(vec![
            ("pathway_enrichment", comp_task("pathway_enrichment")),
            (
                "differential_expression",
                comp_task("differential_expression"),
            ),
        ]);
        let m =
            derive_expected_manifest(&empty_classification(), &dag, ProjectClass::Bioinformatics);
        let entities: Vec<&str> = m.entries.iter().map(|e| e.entity.as_str()).collect();
        let mut sorted = entities.clone();
        sorted.sort();
        assert_eq!(
            entities, sorted,
            "manifest entries must be deterministically sorted"
        );
    }

    #[test]
    fn inject_writes_expected_block_into_policy() {
        let dir = tempfile::tempdir().unwrap();
        let policies = dir.path().join("policies");
        std::fs::create_dir_all(&policies).unwrap();
        // Minimal copy of the policy with an enabled verifiableEntities block.
        std::fs::write(
            policies.join("interpretation-policy.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schemaVersion": "1.1",
                "verifiableEntities": { "enabled": true }
            }))
            .unwrap(),
        )
        .unwrap();

        let manifest = ExpectedClaimManifest {
            schema_version: "1".into(),
            entries: vec![ExpectedClaim {
                entity: "differential_expression".into(),
                contrast: None,
                expected_output_table: Some("differential_expression".into()),
                requirement: Requirement::Required,
                edam_data: None,
            }],
        };
        inject_manifest_into_policy(dir.path(), &manifest).unwrap();

        let raw = std::fs::read_to_string(policies.join("interpretation-policy.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let expected = v["verifiableEntities"]["expected"].as_array().unwrap();
        assert_eq!(expected.len(), 1);
        assert_eq!(expected[0]["requirement"], serde_json::json!("required"));
        assert_eq!(
            expected[0]["expected_output_table"],
            serde_json::json!("differential_expression")
        );
        // Idempotent + deterministic: a second injection of the same manifest
        // produces byte-identical output.
        let first = std::fs::read(policies.join("interpretation-policy.json")).unwrap();
        inject_manifest_into_policy(dir.path(), &manifest).unwrap();
        let second = std::fs::read(policies.join("interpretation-policy.json")).unwrap();
        assert_eq!(first, second, "injection must be idempotent + byte-stable");
    }

    #[test]
    fn inject_is_noop_when_no_entries() {
        let dir = tempfile::tempdir().unwrap();
        let policies = dir.path().join("policies");
        std::fs::create_dir_all(&policies).unwrap();
        let original = serde_json::to_vec_pretty(&serde_json::json!({
            "schemaVersion": "1.1",
            "verifiableEntities": { "enabled": true }
        }))
        .unwrap();
        std::fs::write(policies.join("interpretation-policy.json"), &original).unwrap();

        let empty = ExpectedClaimManifest {
            schema_version: "1".into(),
            entries: vec![],
        };
        inject_manifest_into_policy(dir.path(), &empty).unwrap();

        // Empty manifest still writes an empty `expected: []` so the
        // verifier can distinguish "no expectations" from "block missing".
        let v: serde_json::Value = serde_json::from_slice(
            &std::fs::read(policies.join("interpretation-policy.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(v["verifiableEntities"]["expected"], serde_json::json!([]));
    }
}
