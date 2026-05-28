//! Six audit-proof invariants over the 8 ECAA subgraphs.
//! See `docs/ecaa-spec/invariants.md` for definitions.

pub mod claim_completeness;
pub mod cross_graph_integrity;
pub mod decision_justification;
pub mod equivalence_failure;
pub mod evidence_coverage;
pub mod substrate_validity;
