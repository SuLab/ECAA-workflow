//! Render per-task `provisioning.json` that the install
//! proxy shims read at task runtime.
//!
//! The contract is implemented on the consumer side by `runtime/install-proxy/_common.py`.
//! Each executor calls [`render_provisioning_json`] before invoking the
//! agent so the per-task policy file is on disk; the shims read it via
//! the `SWFC_PROVISIONING_POLICY` env var (or fall back to
//! `/etc/scripps-workflow/provisioning.json` when bind-mounted into the
//! container).
//!
//! The rendered JSON is byte-stable across emits — the install-proxy
//! is a trust boundary, so the same atom-set + safety policy must
//! produce identical `provisioning.json` files. The deterministic
//! ordering comes from `RuntimePrereqs::declared_per_registry()`
//! (BTreeMap) + `BTreeSet`-derived package lists.

use ecaa_workflow_core::atom::ProvisioningPolicy;
use ecaa_workflow_core::dag::Task;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

/// Default allowlisted-mode registries. Operators choosing the
/// `Allowlisted` `ProvisioningPolicy` accept any install whose target
/// registry is in this set. The list mirrors the registry names emitted
/// by `RuntimePrereqs::declared_per_registry()` plus a few common
/// per-language defaults (`bioconda`, `bioconductor`, `npm`, `rubygems`)
/// that real-world atoms reach for.
const DEFAULT_ALLOWED_REGISTRIES: &[&str] = &[
    "apt",
    "bioconda",
    "bioconductor",
    "conda",
    "conda-forge",
    "cran",
    "dnf",
    "npm",
    "pip",
    "rubygems",
];

/// On-disk shape of `provisioning.json`. Matches the Python
/// dataclass `runtime/install-proxy/_common.py::Policy` field-for-field
/// (snake_case, no extra fields). Tested by
/// `safety_render::tests::renders_*` below — the shim's contract
/// relies on these exact keys, so any rename here is a breaking
/// change.
#[derive(Serialize)]
struct ProvisioningJson<'a> {
    provisioning: &'a str,
    atom_id: &'a str,
    declared_packages: BTreeMap<String, Vec<String>>,
    allowed_registries: Vec<String>,
}

/// Render the per-task `provisioning.json` to `out`.
///
/// `declared` is the registry → packages map the install-proxy uses
/// for the `DeclaredOnly` decision. Callers source it from
/// `RuntimePrereqs::declared_per_registry()` on the package-level
/// manifest (`policies/runtime-prereqs.json`).
///
/// Determinism: every collection is a `BTreeMap` or `Vec` sorted by
/// the caller, so two renders of the same task + same declared map
/// produce byte-identical bytes — required because the shim's policy
/// check is a hard trust boundary.
pub fn render_provisioning_json(
    task: &Task,
    declared: BTreeMap<String, Vec<String>>,
    out: &Path,
) -> std::io::Result<()> {
    let s = &task.safety;
    let provisioning_str = match s.provisioning {
        ProvisioningPolicy::Sealed => "sealed",
        ProvisioningPolicy::DeclaredOnly => "declared_only",
        ProvisioningPolicy::Allowlisted => "allowlisted",
    };
    let allowed_registries = if matches!(s.provisioning, ProvisioningPolicy::Allowlisted) {
        DEFAULT_ALLOWED_REGISTRIES
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    } else {
        vec![]
    };
    let atom_id = task.source_atom_id.as_deref().unwrap_or("<unknown>");
    let json = ProvisioningJson {
        provisioning: provisioning_str,
        atom_id,
        declared_packages: declared,
        allowed_registries,
    };
    let text = serde_json::to_string_pretty(&json).map_err(std::io::Error::other)?;
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecaa_workflow_core::atom::{
        CodeExecution, NetworkPolicy, ProvisioningPolicy, SafetyLevel, SafetyPolicy,
        SandboxRequirement,
    };
    use ecaa_workflow_core::dag::{Assignee, ResourceClass, Task, TaskKind, TaskState};

    fn task_with_provisioning(p: ProvisioningPolicy) -> Task {
        Task {
            kind: TaskKind::Computation,
            state: TaskState::Ready,
            depends_on: vec![],
            assignee: Assignee::Agent,
            description: "provisioning render probe".into(),
            spec: None,
            resolution: None,
            result_ref: None,
            resource_class: ResourceClass::CpuHeavy,
            requires_sme_review: false,
            required_artifacts: vec![],
            container: None,
            source_atom_id: Some("test_atom".into()),
            safety: SafetyPolicy {
                level: SafetyLevel::Compute,
                network: NetworkPolicy::None { allowlist: vec![] },
                sandbox: SandboxRequirement::None,
                code_execution: CodeExecution::None,
                provisioning: p,
                controlled_access: false,
            },
        }
    }

    #[test]
    fn renders_sealed_with_empty_allowed_registries() {
        let task = task_with_provisioning(ProvisioningPolicy::Sealed);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        render_provisioning_json(&task, BTreeMap::new(), tmp.path()).unwrap();
        let text = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(text.contains("\"provisioning\": \"sealed\""), "got: {text}");
        assert!(text.contains("\"atom_id\": \"test_atom\""), "got: {text}");
        // Sealed mode locks every install out — even if the atom
        // declared packages, the shim must refuse them all.
        assert!(
            text.contains("\"allowed_registries\": []"),
            "sealed mode must have an empty allowed_registries — got: {text}"
        );
    }

    #[test]
    fn renders_declared_only_with_per_registry_packages() {
        let task = task_with_provisioning(ProvisioningPolicy::DeclaredOnly);
        let mut declared = BTreeMap::new();
        declared.insert(
            "apt".into(),
            vec!["bwa".to_string(), "samtools".to_string()],
        );
        declared.insert("pip".into(), vec!["pandas".to_string()]);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        render_provisioning_json(&task, declared, tmp.path()).unwrap();
        let text = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(
            text.contains("\"provisioning\": \"declared_only\""),
            "got: {text}"
        );
        assert!(text.contains("samtools"), "apt entries missing: {text}");
        assert!(text.contains("bwa"), "apt entries missing: {text}");
        assert!(text.contains("pandas"), "pip entries missing: {text}");
        // declared_only mode is the inverse of allowlisted — the
        // shim consults `declared_packages` only.
        assert!(
            text.contains("\"allowed_registries\": []"),
            "declared_only mode must have empty allowed_registries (the \
             allowlist is the package list itself) — got: {text}"
        );
    }

    #[test]
    fn renders_allowlisted_with_default_registries() {
        let task = task_with_provisioning(ProvisioningPolicy::Allowlisted);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        render_provisioning_json(&task, BTreeMap::new(), tmp.path()).unwrap();
        let text = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(
            text.contains("\"provisioning\": \"allowlisted\""),
            "got: {text}"
        );
        // Allowlisted mode populates the default registry list — the
        // shim accepts installs whose registry is in the allowlist.
        assert!(text.contains("\"conda-forge\""), "got: {text}");
        assert!(text.contains("\"bioconductor\""), "got: {text}");
        assert!(text.contains("\"bioconda\""), "got: {text}");
        assert!(text.contains("\"cran\""), "got: {text}");
        assert!(text.contains("\"pip\""), "got: {text}");
    }

    #[test]
    fn falls_back_to_unknown_atom_id_when_task_lacks_source() {
        let mut task = task_with_provisioning(ProvisioningPolicy::DeclaredOnly);
        task.source_atom_id = None;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        render_provisioning_json(&task, BTreeMap::new(), tmp.path()).unwrap();
        let text = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(
            text.contains("\"atom_id\": \"<unknown>\""),
            "pre-A.S6 packages without source_atom_id must still render \
             a sensible diagnostic id — got: {text}"
        );
    }

    #[test]
    fn render_is_byte_deterministic_across_calls() {
        // Two renders of the same task + same declared map must
        // produce identical bytes — the install-proxy is a trust
        // boundary, so any non-determinism here would let an attacker
        // smuggle different policy decisions across re-emits.
        let task = task_with_provisioning(ProvisioningPolicy::DeclaredOnly);
        let mut declared = BTreeMap::new();
        declared.insert(
            "apt".into(),
            vec!["bwa".to_string(), "samtools".to_string()],
        );
        declared.insert("pip".into(), vec!["pandas".to_string()]);

        let tmp1 = tempfile::NamedTempFile::new().unwrap();
        let tmp2 = tempfile::NamedTempFile::new().unwrap();
        render_provisioning_json(&task, declared.clone(), tmp1.path()).unwrap();
        render_provisioning_json(&task, declared, tmp2.path()).unwrap();

        let a = std::fs::read_to_string(tmp1.path()).unwrap();
        let b = std::fs::read_to_string(tmp2.path()).unwrap();
        assert_eq!(
            a, b,
            "provisioning.json must render byte-identical bytes for \
             the same task + same declared packages"
        );
    }
}
