//! Typed `PolicyRuleId` newtype.
//!
//! Every reference to a policy rule (chain-of-custody record,
//! adversarial lifecycle transition, decision-log entry, population
//! waiver, …) is checked at construction against
//! `config/assumption-policy.yaml`'s `policy_rules:` block. This
//! newtype is the construction-time check.
//!
//! There are two construction paths:
//!
//! * [`PolicyRuleId::new`] — validates the id against an
//!   [`AssumptionPolicyTable`]. Returns
//!   [`PolicyRuleIdError::NotInRegistry`] when the id is unknown.
//!   Use this when the registry is loadable in context (chat-server
//!   waiver POST, planner refusal emission, …).
//!
//! * [`PolicyRuleId::unchecked`] — escape hatch for pure-data
//!   constructors that don't have access to the registry (e.g.
//!   deserializing a `DecisionRecord` from disk, or constructing a
//!   `ChainOfCustody` in a unit test). Deserialization also goes
//!   through this path via `serde(transparent)`; round-tripping an
//!   on-disk record never fails on an unknown id (preserving the
//!   forward-compatibility contract on every serialized record type),
//!   but every site that mints a *new* `PolicyRuleId` from operator
//!   input is responsible for funneling through `new`.
//!
//! A load-time CI gate validates `PolicyRuleId`s against every
//! `DecisionRecord` / `ChainOfCustody` fixture in the corpus.
//! This newtype is the type-level foundation for that gate.

use crate::assumption_policy::AssumptionPolicyTable;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Foreign key into the `policy_rules:` registry in
/// `config/assumption-policy.yaml`. Construct via [`PolicyRuleId::new`]
/// (registry-validated) or [`PolicyRuleId::unchecked`] (legacy / test
/// path).
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    TS,
    Default,
    schemars::JsonSchema,
)]
#[ts(export)]
#[serde(transparent)]
pub struct PolicyRuleId(String);

/// Errors from [`PolicyRuleId::new`].
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum PolicyRuleIdError {
    /// The id is not a registered key in
    /// `assumption-policy.yaml::policy_rules`.
    #[error("policy rule {0:?} not in registry")]
    NotInRegistry(String),
}

impl PolicyRuleId {
    /// Construct a registry-validated id. Returns `Err` if `id` is
    /// not a registered rule in `table`.
    pub fn new(id: &str, table: &AssumptionPolicyTable) -> Result<Self, PolicyRuleIdError> {
        if !table.has_rule(id) {
            return Err(PolicyRuleIdError::NotInRegistry(id.to_string()));
        }
        Ok(PolicyRuleId(id.to_string()))
    }

    /// Construct without consulting the registry. Use only for
    /// deserialization, unit-test fixtures, and pure-data constructors
    /// where the registry isn't reachable.
    pub fn unchecked(id: impl Into<String>) -> Self {
        PolicyRuleId(id.into())
    }

    /// Borrowed view of the underlying string. Used by the audit-log
    /// + RO-Crate emit paths.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PolicyRuleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for PolicyRuleId {
    /// Permissive conversion used by `impl Into<PolicyRuleId>` on
    /// constructors that haven't been migrated to the validated path.
    /// Equivalent to [`PolicyRuleId::unchecked`].
    fn from(s: String) -> Self {
        PolicyRuleId(s)
    }
}

impl From<&str> for PolicyRuleId {
    /// See [`From<String>`] — permissive `Into` for constructor
    /// ergonomics. Equivalent to [`PolicyRuleId::unchecked`].
    fn from(s: &str) -> Self {
        PolicyRuleId(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchecked_is_round_trip_stable() {
        let id = PolicyRuleId::unchecked("x:y");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"x:y\"");
        let back: PolicyRuleId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn display_writes_inner_string() {
        let id = PolicyRuleId::unchecked("rule_foo");
        assert_eq!(format!("{id}"), "rule_foo");
    }
}
