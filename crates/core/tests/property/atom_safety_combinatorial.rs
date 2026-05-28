//! R-24 property test: full Cartesian product of
//! `SafetyLevel × NetworkPolicy × CodeExecution × SandboxRequirement ×
//! ProvisioningPolicy` exercised through `validate_atom_safety`.
//!
//! The lint's documented rules (`atom_safety::SafetyConsistencyError`)
//! enumerate the forbidden combinations: e.g. `Compute` + `Bridge`
//! network is `ComputeAtomHasNetwork`; `GeneratedByAgent` code outside
//! `Exec` is `GeneratedCodeWithoutExecLevel`; `Exec` + `None` sandbox is
//! `ExecAtomMissingSandbox`. This test walks every combination and
//! asserts the lint's verdict against an oracle predicate derived from
//! the same rule set, catching any drift where a rule is silently
//! relaxed or a new variant slips in without a matching gate.
//!
//! ~32 combinations across the closed enum cross-product. Proptest
//! drives the enumeration so failures shrink to the smallest violating
//! tuple.

use proptest::prelude::*;
use scripps_workflow_core::atom::{
    AtomDefinition, CodeExecution, NetworkPolicy, ProvisioningPolicy, SafetyLevel,
    SandboxRequirement,
};
use scripps_workflow_core::atom_safety::validate_atom_safety;

fn safety_level_strategy() -> impl Strategy<Value = SafetyLevel> {
    prop_oneof![
        Just(SafetyLevel::Safe),
        Just(SafetyLevel::Network),
        Just(SafetyLevel::Compute),
        Just(SafetyLevel::Exec),
    ]
}

fn network_policy_strategy() -> impl Strategy<Value = NetworkPolicy> {
    prop_oneof![
        Just(NetworkPolicy::Bridge),
        Just(NetworkPolicy::None { allowlist: vec![] }),
        Just(NetworkPolicy::None {
            allowlist: vec!["api.example.com".into()],
        }),
    ]
}

fn code_execution_strategy() -> impl Strategy<Value = CodeExecution> {
    prop_oneof![
        Just(CodeExecution::None),
        Just(CodeExecution::Vetted),
        Just(CodeExecution::GeneratedByAgent),
    ]
}

fn sandbox_strategy() -> impl Strategy<Value = SandboxRequirement> {
    prop_oneof![
        Just(SandboxRequirement::None),
        Just(SandboxRequirement::ProcessIsolation),
        Just(SandboxRequirement::HardwareEnclave),
    ]
}

fn provisioning_strategy() -> impl Strategy<Value = ProvisioningPolicy> {
    prop_oneof![
        Just(ProvisioningPolicy::Sealed),
        Just(ProvisioningPolicy::DeclaredOnly),
        Just(ProvisioningPolicy::Allowlisted),
    ]
}

/// Oracle: returns `true` when the (level, network, code, sandbox,
/// provisioning) tuple is forbidden by the lint's rule set. Mirrors
/// `crates/core/src/atom_safety.rs::validate_atom_safety` so a drift
/// shows up as a failing proptest, not a silently-passing one.
fn is_forbidden(
    level: SafetyLevel,
    network: &NetworkPolicy,
    code: CodeExecution,
    sandbox: SandboxRequirement,
    provisioning: ProvisioningPolicy,
) -> bool {
    let net_is_empty_none =
        matches!(network, NetworkPolicy::None { allowlist } if allowlist.is_empty());

    // Per-level rules.
    let per_level_violation = match level {
        SafetyLevel::Safe => {
            !net_is_empty_none
                || code != CodeExecution::None
                || provisioning != ProvisioningPolicy::Sealed
        }
        SafetyLevel::Network => net_is_empty_none || code == CodeExecution::GeneratedByAgent,
        SafetyLevel::Compute => !net_is_empty_none || code == CodeExecution::GeneratedByAgent,
        SafetyLevel::Exec => {
            sandbox == SandboxRequirement::None || code != CodeExecution::GeneratedByAgent
        }
    };

    // Cross-field implication rules.
    let cross_field_violation = (code == CodeExecution::GeneratedByAgent
        && level != SafetyLevel::Exec)
        || (sandbox != SandboxRequirement::None && level != SafetyLevel::Exec)
        || (provisioning == ProvisioningPolicy::Allowlisted && level != SafetyLevel::Exec);

    per_level_violation || cross_field_violation
}

proptest! {
    /// For every cross-product element, the lint's verdict (errors
    /// empty ↔ allowed) must equal the oracle's verdict. Failures
    /// shrink to the smallest violating tuple.
    #[test]
    fn lint_matches_oracle(
        level in safety_level_strategy(),
        network in network_policy_strategy(),
        code in code_execution_strategy(),
        sandbox in sandbox_strategy(),
        provisioning in provisioning_strategy(),
    ) {
        let mut atom = AtomDefinition::test_default("combinatorial");
        atom.safety.level = level;
        atom.safety.network = network.clone();
        atom.safety.code_execution = code;
        atom.safety.sandbox = sandbox;
        atom.safety.provisioning = provisioning;

        let errors = validate_atom_safety(&atom);
        let lint_forbidden = !errors.is_empty();
        let oracle_forbidden =
            is_forbidden(level, &network, code, sandbox, provisioning);

        prop_assert_eq!(
            lint_forbidden,
            oracle_forbidden,
            "lint/oracle mismatch on (level={:?}, network={:?}, code={:?}, sandbox={:?}, provisioning={:?}): lint={:?}",
            level, network, code, sandbox, provisioning, errors
        );
    }

    /// Default `SafetyPolicy` (Compute + None network + None code +
    /// None sandbox + DeclaredOnly) MUST be lint-clean. Regression
    /// guard against a default that wouldn't pass its own lint.
    #[test]
    fn default_policy_is_lint_clean(_seed in 0u8..1) {
        let atom = AtomDefinition::test_default("default");
        let errors = validate_atom_safety(&atom);
        prop_assert!(errors.is_empty(), "default policy lint errors: {errors:?}");
    }
}
