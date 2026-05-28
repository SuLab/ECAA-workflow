//! Integration tests for the per-task
//! `provisioning.json` contract the install-proxy shims
//! (`runtime/install-proxy/_common.py`) consume.
//!
//! These tests pin the on-disk JSON shape from outside the harness lib
//! so changes to the rendering surface that would break the Python
//! shim's `load_policy()` contract surface immediately. The shim's
//! `_common.py` parses the same keys (`provisioning`, `atom_id`,
//! `declared_packages`, `allowed_registries`); any rename here is a
//! breaking change.

use ecaa_workflow_core::atom::{
    CodeExecution, NetworkPolicy, ProvisioningPolicy, SafetyLevel, SafetyPolicy, SandboxRequirement,
};
use ecaa_workflow_core::dag::{Assignee, ResourceClass, Task, TaskKind, TaskState};
use ecaa_workflow_harness::safety_render::render_provisioning_json;
use std::collections::BTreeMap;

fn task_with(provisioning: ProvisioningPolicy, atom_id: &str) -> Task {
    Task {
        kind: TaskKind::Computation,
        state: TaskState::Ready,
        depends_on: vec![],
        assignee: Assignee::Agent,
        description: "install-proxy integration probe".into(),
        spec: None,
        resolution: None,
        result_ref: None,
        resource_class: ResourceClass::CpuHeavy,
        requires_sme_review: false,
        required_artifacts: vec![],
        container: None,
        source_atom_id: Some(atom_id.into()),
        safety: SafetyPolicy {
            level: SafetyLevel::Compute,
            network: NetworkPolicy::None { allowlist: vec![] },
            sandbox: SandboxRequirement::None,
            code_execution: CodeExecution::None,
            provisioning,
            controlled_access: false,
        },
    }
}

/// The shim's `_common.load_policy` calls `Path(resolved).read_text()`
/// and `json.loads(...)` — verify the rendered file parses as JSON
/// with every key the shim's dataclass demands.
#[test]
fn rendered_json_round_trips_through_shim_contract() {
    let task = task_with(ProvisioningPolicy::DeclaredOnly, "rnaseq_align_atom");
    let mut declared: BTreeMap<String, Vec<String>> = BTreeMap::new();
    declared.insert(
        "apt".into(),
        vec!["bwa".to_string(), "samtools".to_string()],
    );
    declared.insert(
        "pip".into(),
        vec!["pandas".to_string(), "scanpy".to_string()],
    );

    let tmp = tempfile::NamedTempFile::new().unwrap();
    render_provisioning_json(&task, declared, tmp.path()).unwrap();
    let text = std::fs::read_to_string(tmp.path()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("must be valid JSON");

    // Verify every field the Python `Policy` dataclass expects.
    assert_eq!(
        parsed.get("provisioning").and_then(|v| v.as_str()),
        Some("declared_only"),
        "shim consults `provisioning` key for the policy string"
    );
    assert_eq!(
        parsed.get("atom_id").and_then(|v| v.as_str()),
        Some("rnaseq_align_atom"),
        "shim records the atom_id in install-log entries"
    );
    let dp = parsed
        .get("declared_packages")
        .and_then(|v| v.as_object())
        .expect("declared_packages must be a JSON object");
    let apt_list = dp.get("apt").and_then(|v| v.as_array()).expect("apt list");
    assert_eq!(apt_list.len(), 2);
    let pip_list = dp.get("pip").and_then(|v| v.as_array()).expect("pip list");
    assert_eq!(pip_list.len(), 2);
    let allowed = parsed
        .get("allowed_registries")
        .and_then(|v| v.as_array())
        .expect("allowed_registries must be present");
    assert!(
        allowed.is_empty(),
        "declared_only mode has empty allowed_registries"
    );
}

/// Sealed mode: shim refuses every install — verify the rendered JSON
/// matches this contract.
#[test]
fn sealed_mode_renders_empty_allowed_and_declared() {
    let task = task_with(ProvisioningPolicy::Sealed, "validate_atom");
    let tmp = tempfile::NamedTempFile::new().unwrap();
    render_provisioning_json(&task, BTreeMap::new(), tmp.path()).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(tmp.path()).unwrap()).expect("must parse");
    assert_eq!(
        parsed.get("provisioning").and_then(|v| v.as_str()),
        Some("sealed")
    );
    let allowed = parsed
        .get("allowed_registries")
        .and_then(|v| v.as_array())
        .unwrap();
    assert!(allowed.is_empty(), "sealed → allowed_registries must be []");
    let declared = parsed
        .get("declared_packages")
        .and_then(|v| v.as_object())
        .unwrap();
    assert!(
        declared.is_empty(),
        "no declared packages passed → empty declared_packages map"
    );
}

/// Allowlisted mode: shim accepts installs whose registry is in the
/// allowed_registries list. The default list must include the
/// production-relevant registries (apt/pip/conda/cran/bioconda/etc.).
#[test]
fn allowlisted_mode_includes_default_registries() {
    let task = task_with(ProvisioningPolicy::Allowlisted, "lit_fetch_atom");
    let tmp = tempfile::NamedTempFile::new().unwrap();
    render_provisioning_json(&task, BTreeMap::new(), tmp.path()).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(tmp.path()).unwrap()).expect("must parse");
    assert_eq!(
        parsed.get("provisioning").and_then(|v| v.as_str()),
        Some("allowlisted")
    );
    let allowed: Vec<String> = parsed
        .get("allowed_registries")
        .and_then(|v| v.as_array())
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    for reg in ["apt", "pip", "conda", "conda-forge", "bioconda", "cran"] {
        assert!(
            allowed.contains(&reg.to_string()),
            "default allowlist must include `{}`; got {:?}",
            reg,
            allowed
        );
    }
}

/// Confirm the file is created when parent dirs do not yet exist. The
/// harness writes to `runtime/inputs/<task_id>/provisioning.json`,
/// which may not exist before the agent runs.
#[test]
fn render_creates_missing_parent_directories() {
    let tmp_root = tempfile::tempdir().unwrap();
    let nested = tmp_root
        .path()
        .join("runtime/inputs/some_task/provisioning.json");
    assert!(
        !nested.parent().unwrap().exists(),
        "test precondition: parent dir should not exist yet"
    );
    let task = task_with(ProvisioningPolicy::DeclaredOnly, "atom");
    render_provisioning_json(&task, BTreeMap::new(), &nested)
        .expect("render must create parent dirs");
    assert!(nested.exists(), "provisioning.json was written");
}
