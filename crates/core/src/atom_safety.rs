//! Registry-load lint for atom safety consistency.

use crate::atom::{
    AtomDefinition, CodeExecution, ContainerSource, NetworkPolicy, ProvisioningPolicy, SafetyLevel,
    SandboxRequirement,
};
use crate::ids::AtomId;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
/// SafetyConsistencyError discriminant.
pub enum SafetyConsistencyError {
    #[error("atom {atom_id}: Safe level requires no network egress, found {found:?}")]
    /// SafeAtomHasNetwork variant.
    SafeAtomHasNetwork {
        atom_id: AtomId,
        found: NetworkPolicy,
    },

    #[error("atom {atom_id}: Safe level forbids code execution, found {found:?}")]
    /// SafeAtomHasCodeExecution variant.
    SafeAtomHasCodeExecution {
        atom_id: AtomId,
        found: CodeExecution,
    },

    #[error("atom {atom_id}: Safe level requires Sealed provisioning, found {found:?}")]
    /// SafeAtomProvisioningNotSealed variant.
    SafeAtomProvisioningNotSealed {
        atom_id: AtomId,
        found: ProvisioningPolicy,
    },

    #[error("atom {atom_id}: Compute level requires no network egress, found {found:?}")]
    /// ComputeAtomHasNetwork variant.
    ComputeAtomHasNetwork {
        atom_id: AtomId,
        found: NetworkPolicy,
    },

    #[error("atom {atom_id}: Compute level forbids GeneratedByAgent code (use Exec)")]
    ComputeAtomGeneratedCode { atom_id: AtomId },

    #[error(
        "atom {atom_id}: Network level requires non-empty allowlist or Bridge, found empty None"
    )]
    NetworkAtomNoEgress { atom_id: AtomId },

    #[error("atom {atom_id}: Network level forbids GeneratedByAgent code (use Exec)")]
    NetworkAtomGeneratedCode { atom_id: AtomId },

    #[error("atom {atom_id}: Exec level requires sandbox != None, found None")]
    ExecAtomMissingSandbox { atom_id: AtomId },

    #[error(
        "atom {atom_id}: Exec level requires code_execution == GeneratedByAgent, found {found:?}"
    )]
    /// ExecAtomMissingGeneratedCode variant.
    ExecAtomMissingGeneratedCode {
        atom_id: AtomId,
        found: CodeExecution,
    },

    #[error("atom {atom_id}: code_execution == GeneratedByAgent requires level == Exec, found {found:?}")]
    GeneratedCodeWithoutExecLevel { atom_id: AtomId, found: SafetyLevel },

    #[error("atom {atom_id}: sandbox != None requires level == Exec, found {found:?}")]
    SandboxWithoutExecLevel { atom_id: AtomId, found: SafetyLevel },

    #[error("atom {atom_id}: provisioning == Allowlisted requires level == Exec, found {found:?}")]
    AllowlistedProvisioningWithoutExecLevel { atom_id: AtomId, found: SafetyLevel },

    // Note: the field is named `container_source` rather than `source`
    // because `thiserror` treats a field literally named `source` as the
    // wrapped-cause field, requiring it to implement `std::error::Error`.
    // The print-format `{container_source:?}` keeps the message identical
    // to the design doc's "ContainerSource <kind>" phrasing.
    #[error("atom {atom_id}: ContainerSource {container_source:?} requires provisioning == Sealed, found {found:?}")]
    /// NonImageSourceRequiresSealed variant.
    NonImageSourceRequiresSealed {
        atom_id: AtomId,
        container_source: ContainerSource,
        /// Found.
        found: ProvisioningPolicy,
    },

    #[error(
        "atom {atom_id}: ContainerSpec.network is deprecated and conflicts with safety.network"
    )]
    ContainerNetworkOverride { atom_id: AtomId },

    #[error("atom {atom_id}: ContainerSource::Host forbids SafetyLevel::Exec — agent-generated code without container isolation can escape to the host")]
    HostContainerWithExecLevel { atom_id: AtomId },

    #[error("atom {atom_id}: ContainerSource::{kind} not supported by any production executor; declare an `Image` source or use a derived-image build")]
    UnsupportedContainerSource { atom_id: AtomId, kind: &'static str },
}

/// Apply per-level + cross-field consistency rules. Returns ALL
/// violations on the atom (vec, not first-fail) so the registry can
/// surface every issue at once.
#[must_use = "atom-safety violations must be inspected — dropping the Vec lets an inconsistent SafetyPolicy reach dispatch where `enforce_safety_policy` may not catch every shape"]
pub fn validate_atom_safety(atom: &AtomDefinition) -> Vec<SafetyConsistencyError> {
    let mut errors = Vec::new();
    let id = || AtomId::from(atom.id.as_str());
    let s = &atom.safety;

    // Per-level rules.
    match s.level {
        SafetyLevel::Safe => {
            if !is_empty_none(&s.network) {
                errors.push(SafetyConsistencyError::SafeAtomHasNetwork {
                    atom_id: id(),
                    found: s.network.clone(),
                });
            }
            if s.code_execution != CodeExecution::None {
                errors.push(SafetyConsistencyError::SafeAtomHasCodeExecution {
                    atom_id: id(),
                    found: s.code_execution,
                });
            }
            if s.provisioning != ProvisioningPolicy::Sealed {
                errors.push(SafetyConsistencyError::SafeAtomProvisioningNotSealed {
                    atom_id: id(),
                    found: s.provisioning,
                });
            }
        }
        SafetyLevel::Network => {
            if is_empty_none(&s.network) {
                errors.push(SafetyConsistencyError::NetworkAtomNoEgress { atom_id: id() });
            }
            if s.code_execution == CodeExecution::GeneratedByAgent {
                errors.push(SafetyConsistencyError::NetworkAtomGeneratedCode { atom_id: id() });
            }
        }
        SafetyLevel::Compute => {
            if !is_empty_none(&s.network) {
                errors.push(SafetyConsistencyError::ComputeAtomHasNetwork {
                    atom_id: id(),
                    found: s.network.clone(),
                });
            }
            if s.code_execution == CodeExecution::GeneratedByAgent {
                errors.push(SafetyConsistencyError::ComputeAtomGeneratedCode { atom_id: id() });
            }
        }
        SafetyLevel::Exec => {
            if s.sandbox == SandboxRequirement::None {
                errors.push(SafetyConsistencyError::ExecAtomMissingSandbox { atom_id: id() });
            }
            if s.code_execution != CodeExecution::GeneratedByAgent {
                errors.push(SafetyConsistencyError::ExecAtomMissingGeneratedCode {
                    atom_id: id(),
                    found: s.code_execution,
                });
            }
        }
    }

    // Cross-field implication rules.
    if s.code_execution == CodeExecution::GeneratedByAgent && s.level != SafetyLevel::Exec {
        errors.push(SafetyConsistencyError::GeneratedCodeWithoutExecLevel {
            atom_id: id(),
            found: s.level,
        });
    }
    if s.sandbox != SandboxRequirement::None && s.level != SafetyLevel::Exec {
        errors.push(SafetyConsistencyError::SandboxWithoutExecLevel {
            atom_id: id(),
            found: s.level,
        });
    }
    if s.provisioning == ProvisioningPolicy::Allowlisted && s.level != SafetyLevel::Exec {
        errors.push(
            SafetyConsistencyError::AllowlistedProvisioningWithoutExecLevel {
                atom_id: id(),
                found: s.level,
            },
        );
    }

    // R2.6 — refuse non-Image ContainerSource variants at registry
    // load. No production executor (Local / AWS / SLURM) materializes
    // ContainerSource::Conda (Wave/Seqera resolver path was never
    // wired) or ContainerSource::Host (no harness invokes it). Surface
    // a clear "declare an `Image` source or use a derived-image build"
    // diagnostic instead of letting the atom flow to dispatch where
    // the failure mode is silent.
    if let Some(container) = &atom.preferred_container {
        let kind = match container.source {
            ContainerSource::Image => None,
            ContainerSource::Conda { .. } => Some("Conda"),
            ContainerSource::Host => Some("Host"),
        };
        if let Some(k) = kind {
            errors.push(SafetyConsistencyError::UnsupportedContainerSource {
                atom_id: id(),
                kind: k,
            });
        }
        // ContainerSource::Host means bare-host execution with no
        // container isolation. An atom with SafetyLevel::Exec runs
        // agent-generated code (the LLM authors a script that gets
        // shelled out at runtime); without container isolation that
        // code can read/write host filesystem state outside the
        // package scope and exfil credentials. Refuse the combination
        // at registry load. The `--no-sandbox`-style override (if it
        // ever exists) must live elsewhere with its own gate.
        if matches!(container.source, ContainerSource::Host) && s.level == SafetyLevel::Exec {
            errors.push(SafetyConsistencyError::HostContainerWithExecLevel { atom_id: id() });
        }
        // ContainerSpec.network deprecated conflict — flag when set.
        // The field itself is `#[deprecated]`; this lint is the
        // load-bearing reader that detects stragglers in atom YAML,
        // so it must keep reading the deprecated field.
        #[allow(deprecated)]
        if container.network.is_some() {
            errors.push(SafetyConsistencyError::ContainerNetworkOverride { atom_id: id() });
        }
    }

    errors
}

/// True when `n` is `NetworkPolicy::None` with an empty allowlist —
/// the "deny-all" form. Used by the Safe / Compute rules.
fn is_empty_none(n: &NetworkPolicy) -> bool {
    matches!(n, NetworkPolicy::None { allowlist } if allowlist.is_empty())
}

/// Aggregate per-package safety summary
/// surfaced through `PackageSafetyBanner` in the UI. `worst_case_level`
/// is the maximum-severity level across the package's tasks (ordered
/// Safe < Network < Compute < Exec); `level_counts` is the per-level
/// task tally so the SME can spot a single Exec task in a Compute-heavy
/// package at a glance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct SafetySummary {
    /// Worst case level.
    pub worst_case_level: SafetyLevel,
    /// Level counts.
    pub level_counts: SafetyLevelCounts,
}

#[derive(
    Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema,
)]
#[ts(export)]
/// SafetyLevelCounts data.
pub struct SafetyLevelCounts {
    /// Safe.
    pub safe: u32,
    /// Network.
    pub network: u32,
    /// Compute.
    pub compute: u32,
    /// Exec.
    pub exec: u32,
}

impl SafetyLevelCounts {
    /// Increment.
    pub fn increment(&mut self, level: SafetyLevel) {
        match level {
            SafetyLevel::Safe => self.safe += 1,
            SafetyLevel::Network => self.network += 1,
            SafetyLevel::Compute => self.compute += 1,
            SafetyLevel::Exec => self.exec += 1,
        }
    }
}

impl SafetySummary {
    /// Build a SafetySummary from any iterator of SafetyLevels (one per
    /// task). Empty input falls back to the default Compute level — the
    /// same Default `SafetyLevel` value used elsewhere in the system so
    /// pre-A.S6 / empty packages don't surface as anomalous.
    pub fn from_levels<I: IntoIterator<Item = SafetyLevel>>(levels: I) -> Self {
        let mut counts = SafetyLevelCounts::default();
        let mut worst = SafetyLevel::Safe;
        let mut saw_any = false;
        for lvl in levels {
            saw_any = true;
            counts.increment(lvl);
            if level_rank(lvl) > level_rank(worst) {
                worst = lvl;
            }
        }
        // Default SafetyLevel is Compute; preserve that when the package
        // contains no tasks so the UI doesn't render a Safe-by-default
        // banner that misrepresents an empty/aborted package.
        if !saw_any {
            worst = SafetyLevel::default();
        }
        Self {
            worst_case_level: worst,
            level_counts: counts,
        }
    }
}

/// Severity ordering for `worst_case_level` computation. Higher number
/// == more permissive (more attention from the SME). Exec > Compute >
/// Network > Safe matches the design doc §A.S6.
fn level_rank(level: SafetyLevel) -> u8 {
    match level {
        SafetyLevel::Safe => 0,
        SafetyLevel::Network => 1,
        SafetyLevel::Compute => 2,
        SafetyLevel::Exec => 3,
    }
}

// ── Grant v19 §Authentication of Key Resources — D3 ────────────────
//
// Per-package aggregation of every atom's SafetyPolicy. Written to
// `runtime/security-policy.json` at emit time so reviewers see the
// full sandbox profile / network policy / code-execution stance /
// sandbox requirement / provisioning policy mix.

/// Top-level payload for `runtime/security-policy.json`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PackageSafetyAggregate {
    /// Schema version.
    pub schema_version: String,
    /// Atom policies.
    pub atom_policies: Vec<AtomPolicyEntry>,
    /// Package max safety level.
    pub package_max_safety_level: String,
    /// Package max network policy.
    pub package_max_network_policy: String,
    /// Package requires sandbox.
    pub package_requires_sandbox: bool,
    /// Container image digests.
    pub container_image_digests: Vec<String>,
    /// Scan results summary.
    pub scan_results_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
/// AtomPolicyEntry data.
pub struct AtomPolicyEntry {
    pub atom_id: AtomId,
    pub safety_level: String,
    /// Network policy.
    pub network_policy: String,
    /// Code execution.
    pub code_execution: String,
    /// Sandbox requirement.
    pub sandbox_requirement: String,
    /// Provisioning policy.
    pub provisioning_policy: String,
}

/// Build a [`PackageSafetyAggregate`] from a slice of atoms in use by
/// the package + the container image digests at emit time.
///
/// The max-rollups use the [`level_rank`] ordering for safety levels
/// (Exec > Compute > Network > Safe) and a conservative heuristic for
/// the network policy: any non-`None` variant promotes the package
/// rollup to `"Bridge"` (the most permissive non-restricted option).
/// If the underlying enums gain new variants between emits the
/// heuristic may need updating — this is documented inline at each
/// rollup site.
pub fn aggregate_for_package(
    atoms_in_use: &[&AtomDefinition],
    container_image_digests: Vec<String>,
) -> PackageSafetyAggregate {
    let mut entries: Vec<AtomPolicyEntry> = Vec::with_capacity(atoms_in_use.len());
    let mut worst_level = SafetyLevel::Safe;
    let mut saw_any = false;
    let mut max_network: &str = "None";
    let mut needs_sandbox = false;

    for atom in atoms_in_use {
        let p = &atom.safety;
        entries.push(AtomPolicyEntry {
            atom_id: AtomId::from(atom.id.as_str()),
            safety_level: format!("{:?}", p.level),
            network_policy: format!("{:?}", p.network),
            code_execution: format!("{:?}", p.code_execution),
            sandbox_requirement: format!("{:?}", p.sandbox),
            provisioning_policy: format!("{:?}", p.provisioning),
        });

        // Safety-level max-rollup uses the established level_rank
        // ordering (Exec > Compute > Network > Safe). Adding a new
        // SafetyLevel variant requires updating both level_rank and
        // this site.
        saw_any = true;
        if level_rank(p.level) > level_rank(worst_level) {
            worst_level = p.level;
        }

        // Network policy rollup: any non-None variant promotes to
        // Bridge (the most permissive). Conservative — adding a more
        // permissive variant (e.g. Open) would still report Bridge
        // here, which is a slight under-statement; update if the
        // enum grows beyond Bridge/AllowList/None.
        if !matches!(p.network, NetworkPolicy::None { .. }) {
            max_network = "Bridge";
        }

        if !matches!(p.sandbox, SandboxRequirement::None) {
            needs_sandbox = true;
        }
    }

    // Default SafetyLevel (Compute) when no atoms are in use so an
    // empty package doesn't surface as anomalous-Safe. Mirrors the
    // SafetySummary::from_levels invariant above.
    if !saw_any {
        worst_level = SafetyLevel::default();
    }

    PackageSafetyAggregate {
        schema_version: "1".into(),
        atom_policies: entries,
        package_max_safety_level: format!("{:?}", worst_level),
        package_max_network_policy: max_network.into(),
        package_requires_sandbox: needs_sandbox,
        container_image_digests,
        // Populated when vulnerability-scan integration lands; nil
        // until then so the field is present in every emitted payload.
        scan_results_summary: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::{AtomDefinition, ContainerSpec, NetworkPolicy, SafetyPolicy};

    fn safe_atom() -> AtomDefinition {
        let mut a = AtomDefinition::test_default("safe1");
        a.safety = SafetyPolicy {
            level: SafetyLevel::Safe,
            network: NetworkPolicy::None { allowlist: vec![] },
            code_execution: CodeExecution::None,
            sandbox: SandboxRequirement::None,
            provisioning: ProvisioningPolicy::Sealed,
            controlled_access: false,
        };
        a
    }

    fn compute_atom() -> AtomDefinition {
        let mut a = AtomDefinition::test_default("compute1");
        a.safety = SafetyPolicy {
            level: SafetyLevel::Compute,
            network: NetworkPolicy::None { allowlist: vec![] },
            code_execution: CodeExecution::Vetted,
            sandbox: SandboxRequirement::None,
            provisioning: ProvisioningPolicy::DeclaredOnly,
            controlled_access: false,
        };
        a
    }

    fn network_atom() -> AtomDefinition {
        let mut a = AtomDefinition::test_default("net1");
        a.safety = SafetyPolicy {
            level: SafetyLevel::Network,
            network: NetworkPolicy::None {
                allowlist: vec!["api.ncbi.nlm.nih.gov".into()],
            },
            code_execution: CodeExecution::None,
            sandbox: SandboxRequirement::None,
            provisioning: ProvisioningPolicy::DeclaredOnly,
            controlled_access: false,
        };
        a
    }

    fn exec_atom() -> AtomDefinition {
        let mut a = AtomDefinition::test_default("exec1");
        a.safety = SafetyPolicy {
            level: SafetyLevel::Exec,
            network: NetworkPolicy::Bridge,
            code_execution: CodeExecution::GeneratedByAgent,
            sandbox: SandboxRequirement::ProcessIsolation,
            provisioning: ProvisioningPolicy::Allowlisted,
            controlled_access: false,
        };
        a
    }

    #[test]
    fn safe_atom_passes() {
        assert!(validate_atom_safety(&safe_atom()).is_empty());
    }

    #[test]
    fn compute_atom_passes() {
        assert!(validate_atom_safety(&compute_atom()).is_empty());
    }

    #[test]
    fn network_atom_passes() {
        assert!(validate_atom_safety(&network_atom()).is_empty());
    }

    #[test]
    fn exec_atom_passes() {
        assert!(validate_atom_safety(&exec_atom()).is_empty());
    }

    #[test]
    fn default_atom_passes() {
        // Task 1.1 implemented a custom Default impl so the default
        // policy is { Compute / None{[]} / None / None / DeclaredOnly } —
        // which lint-passes. This test pins that contract.
        let a = AtomDefinition::test_default("default1");
        let errors = validate_atom_safety(&a);
        assert!(
            errors.is_empty(),
            "default safety policy fails lint: {errors:?}"
        );
    }

    #[test]
    fn safe_atom_with_network_fails() {
        let mut a = safe_atom();
        a.safety.network = NetworkPolicy::Bridge;
        let errors = validate_atom_safety(&a);
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors[0],
            SafetyConsistencyError::SafeAtomHasNetwork { .. }
        ));
    }

    #[test]
    fn compute_atom_with_network_fails() {
        let mut a = compute_atom();
        a.safety.network = NetworkPolicy::Bridge;
        let errors = validate_atom_safety(&a);
        assert!(errors
            .iter()
            .any(|e| matches!(e, SafetyConsistencyError::ComputeAtomHasNetwork { .. })));
    }

    #[test]
    fn network_atom_with_empty_none_fails() {
        let mut a = network_atom();
        a.safety.network = NetworkPolicy::None { allowlist: vec![] };
        let errors = validate_atom_safety(&a);
        assert!(errors
            .iter()
            .any(|e| matches!(e, SafetyConsistencyError::NetworkAtomNoEgress { .. })));
    }

    #[test]
    fn exec_atom_missing_sandbox_fails() {
        let mut a = exec_atom();
        a.safety.sandbox = SandboxRequirement::None;
        let errors = validate_atom_safety(&a);
        assert!(errors
            .iter()
            .any(|e| matches!(e, SafetyConsistencyError::ExecAtomMissingSandbox { .. })));
    }

    #[test]
    fn exec_atom_missing_generated_code_fails() {
        let mut a = exec_atom();
        a.safety.code_execution = CodeExecution::Vetted;
        let errors = validate_atom_safety(&a);
        assert!(errors.iter().any(|e| matches!(
            e,
            SafetyConsistencyError::ExecAtomMissingGeneratedCode { .. }
        )));
    }

    #[test]
    fn generated_code_without_exec_level_fails() {
        let mut a = compute_atom();
        a.safety.code_execution = CodeExecution::GeneratedByAgent;
        let errors = validate_atom_safety(&a);
        assert!(errors.iter().any(|e| matches!(
            e,
            SafetyConsistencyError::GeneratedCodeWithoutExecLevel { .. }
        )));
    }

    #[test]
    fn sandbox_without_exec_level_fails() {
        let mut a = compute_atom();
        a.safety.sandbox = SandboxRequirement::ProcessIsolation;
        let errors = validate_atom_safety(&a);
        assert!(errors
            .iter()
            .any(|e| matches!(e, SafetyConsistencyError::SandboxWithoutExecLevel { .. })));
    }

    #[test]
    fn allowlisted_without_exec_level_fails() {
        let mut a = compute_atom();
        a.safety.provisioning = ProvisioningPolicy::Allowlisted;
        let errors = validate_atom_safety(&a);
        assert!(errors.iter().any(|e| matches!(
            e,
            SafetyConsistencyError::AllowlistedProvisioningWithoutExecLevel { .. }
        )));
    }

    #[test]
    #[allow(deprecated)]
    fn conda_source_is_refused() {
        use std::collections::BTreeMap;
        let mut a = compute_atom();
        a.preferred_container = Some(ContainerSpec {
            image: "ghcr.io/x/y".into(),
            tag: "0.1".into(),
            digest: String::new(),
            arch: vec!["amd64".into()],
            gpu_required: false,
            network: None,
            source: ContainerSource::Conda {
                conda_packages: BTreeMap::new(),
            },
        });
        let errors = validate_atom_safety(&a);
        assert!(errors.iter().any(|e| matches!(
            e,
            SafetyConsistencyError::UnsupportedContainerSource { kind: "Conda", .. }
        )));
    }

    #[test]
    #[allow(deprecated)]
    fn host_source_is_refused() {
        let mut a = compute_atom();
        a.preferred_container = Some(ContainerSpec {
            image: "ghcr.io/x/y".into(),
            tag: "0.1".into(),
            digest: String::new(),
            arch: vec!["amd64".into()],
            gpu_required: false,
            network: None,
            source: ContainerSource::Host,
        });
        let errors = validate_atom_safety(&a);
        assert!(errors.iter().any(|e| matches!(
            e,
            SafetyConsistencyError::UnsupportedContainerSource { kind: "Host", .. }
        )));
    }

    #[test]
    #[allow(deprecated)]
    fn deprecated_container_network_flagged() {
        let mut a = compute_atom();
        a.preferred_container = Some(ContainerSpec {
            image: "ghcr.io/x/y".into(),
            tag: "0.1".into(),
            digest: String::new(),
            arch: vec!["amd64".into()],
            gpu_required: false,
            network: Some(NetworkPolicy::Bridge),
            source: ContainerSource::Image,
        });
        let errors = validate_atom_safety(&a);
        assert!(errors
            .iter()
            .any(|e| matches!(e, SafetyConsistencyError::ContainerNetworkOverride { .. })));
    }

    // ---- SafetySummary tests ----

    #[test]
    fn safety_summary_empty_input_defaults_to_compute() {
        let s = SafetySummary::from_levels(std::iter::empty());
        // Default SafetyLevel is Compute (matches atom default).
        assert_eq!(s.worst_case_level, SafetyLevel::Compute);
        assert_eq!(s.level_counts, SafetyLevelCounts::default());
    }

    #[test]
    fn safety_summary_picks_worst_case_exec() {
        let levels = vec![
            SafetyLevel::Safe,
            SafetyLevel::Compute,
            SafetyLevel::Compute,
            SafetyLevel::Exec,
            SafetyLevel::Network,
        ];
        let s = SafetySummary::from_levels(levels);
        assert_eq!(s.worst_case_level, SafetyLevel::Exec);
        assert_eq!(s.level_counts.safe, 1);
        assert_eq!(s.level_counts.compute, 2);
        assert_eq!(s.level_counts.exec, 1);
        assert_eq!(s.level_counts.network, 1);
    }

    #[test]
    fn safety_summary_all_safe_yields_safe_worst_case() {
        let levels = vec![SafetyLevel::Safe, SafetyLevel::Safe];
        let s = SafetySummary::from_levels(levels);
        assert_eq!(s.worst_case_level, SafetyLevel::Safe);
        assert_eq!(s.level_counts.safe, 2);
    }
}
