//! Typestate witness for the planner's facet-validation pass (v4 D1 / F23).
//!
//! Two zero-sized state markers — `Unvalidated` and `Validated` —
//! parameterize `PortInPass<S>` so the compiler refuses to feed an
//! unvalidated contract into a slot that requires facet checks to
//! have run. The pattern mirrors the well-known `BufferState` trick
//! from typestate Rust and prevents the "I forgot to call
//! `check_facets()`" class of bug at compile time.
//!
//! NEVER derive `Serialize`, `Deserialize`, or `TS` on these types.

use std::marker::PhantomData;

use crate::composer_v4::PlanningContext;
use crate::workflow_contracts::port::PortContract;

/// Typestate marker: contract has NOT been validated by
/// `check_facets`.
pub(crate) struct Unvalidated;
/// Typestate marker: contract has passed `check_facets`.
pub(crate) struct Validated;

/// Soft error for `PortInPass::check_facets`. The real verifier
/// engine has its own richer error hierarchy; this local enum is
/// a placeholder for the call-site adopter to swap with the
/// engine-specific error when it actually wires up the typestate
/// helper.
#[derive(Debug)]
pub(crate) enum PassError {
    /// Catch-all: the contract failed at least one facet check.
    #[allow(dead_code)]
    FacetCheckFailed(String),
}

/// Wraps a `PortContract` together with a phantom typestate `S`
/// asserting whether the contract has been through `check_facets`.
///
/// `pub(crate)` by F23 mandate.
pub(crate) struct PortInPass<S> {
    contract: PortContract,
    _state: PhantomData<S>,
}

impl PortInPass<Unvalidated> {
    pub(crate) fn new(contract: PortContract) -> Self {
        Self {
            contract,
            _state: PhantomData,
        }
    }

    /// Runs facet validation against the planning context. On
    /// success returns a `PortInPass<Validated>` that downstream
    /// helpers can consume; on failure returns a `PassError`.
    ///
    /// The default implementation is a placeholder — call sites
    /// that adopt typestate substitute their domain-specific
    /// validator here. The signature is what matters for F23: the
    /// `Validated` state can only be constructed by going through
    /// this method.
    pub(crate) fn check_facets(
        self,
        _ctx: &PlanningContext,
    ) -> Result<PortInPass<Validated>, PassError> {
        Ok(PortInPass {
            contract: self.contract,
            _state: PhantomData,
        })
    }
}

impl PortInPass<Validated> {
    pub(crate) fn contract(&self) -> &PortContract {
        &self.contract
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::semantic_type::SemanticType;

    fn dummy_port() -> PortContract {
        PortContract {
            name: "test-port".to_string(),
            semantic_type: SemanticType::edam("data:0863", "test"),
            ..PortContract::default()
        }
    }

    #[test]
    fn unvalidated_to_validated_typestate_transition() {
        let port = dummy_port();
        let pass: PortInPass<Unvalidated> = PortInPass::new(port);
        let ctx = PlanningContext::default();
        let validated = pass.check_facets(&ctx).expect("check_facets ok");
        assert_eq!(validated.contract().name, "test-port");
    }
}
