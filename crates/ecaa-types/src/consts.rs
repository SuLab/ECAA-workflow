//! Canonical const arrays defining ECAA v0.1 closed type-set sizes.
//!
//! Source of truth for cross-doc consistency checks. Spec files MUST
//! reference exactly these names; downstream linters import this module
//! instead of hardcoding string lists.

/// The 25 node-type names in canonical form (matches `v0.1.md` §5
/// sub-graph table inline code spans).
pub const NODE_TYPES: &[&str] = &[
    // I (5)
    "Question",
    "Cohort",
    "Contrast",
    "Modality",
    "ExpectedOutput",
    // D (4)
    "MethodChoice",
    "Justification",
    "Alternative",
    "Citation",
    // E (5)
    "WorkflowStep",
    "Container",
    "InputFile",
    "OutputFile",
    "RuntimeEnvironment",
    // V (4)
    "Table",
    "Figure",
    "Statistic",
    "File",
    // C (3)
    "Claim",
    "Quantification",
    "Direction",
    // Q (1)
    "RerunOutcome",
    // F (2)
    "Blocker",
    "RecoveryAction",
    // A (1)
    "InvariantVerdict",
];

/// The 20 edge-predicate names in canonical form (snake_case wire format).
pub const EDGE_PREDICATES: &[&str] = &[
    // I (3)
    "refines",
    "stratifies",
    "expects",
    // D (5)
    "chooses",
    "rejects",
    "cites",
    "amends",
    "prov:wasDerivedFrom",
    // E (3)
    "produces",
    "consumes",
    "runs_in",
    // V (2)
    "appears_in",
    "computed_from",
    // C (2)
    "supported_by",
    "contradicts",
    // Q (2)
    "equivalent_to",
    "diverges_from",
    // F (2)
    "requires",
    "unblocks",
    // A (1)
    "evaluated_against",
];

/// The 6 normative invariant IDs (snake_case wire format, matching
/// `InvariantId` serde rename).
pub const INVARIANT_IDS: &[&str] = &[
    "claim_completeness",
    "decision_justification",
    "evidence_coverage",
    "equivalence_failure",
    "cross_graph_integrity",
    "substrate_validity",
];

/// The 8 required sidecar paths, in `(letter, path)` form.
pub const SIDECAR_PATHS: &[(&str, &str)] = &[
    ("I", "runtime/intake-conversation.jsonl"),
    ("D", "runtime/decisions.jsonl"),
    ("E", "runtime/validation-reports.jsonl"),
    ("V", "runtime/proofs.jsonl"),
    ("C", "runtime/claim-verification.json"),
    ("Q", "runtime/verifier-decisions.jsonl"),
    ("F", "runtime/assumptions.jsonl"),
    ("A", "runtime/audit-proof-report.json"),
];

/// The 6 normative `conformsTo` profile IRIs.
pub const REQUIRED_PROFILE_IRIS: &[&str] = &[
    "https://w3id.org/ro/crate/1.1",
    "https://w3id.org/workflowhub/workflow-ro-crate/1.0",
    "https://w3id.org/ro/wfrun/process/0.5",
    "https://w3id.org/ro/wfrun/workflow/0.5",
    "https://w3id.org/ro/wfrun/provenance/0.5",
    "https://w3id.org/ecaa/v0.1",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invariants::InvariantId;

    #[test]
    fn closed_set_sizes_match_spec() {
        assert_eq!(NODE_TYPES.len(), 25, "spec: 25 node types");
        assert_eq!(EDGE_PREDICATES.len(), 20, "spec: 20 edge predicates");
        assert_eq!(INVARIANT_IDS.len(), 6, "spec: 6 invariants");
        assert_eq!(SIDECAR_PATHS.len(), 8, "spec: 8 sub-graphs");
        assert_eq!(REQUIRED_PROFILE_IRIS.len(), 6, "spec: 6 profile IRIs");
    }

    /// Catch string-form drift between the typed `InvariantId::ALL`
    /// enum and the `INVARIANT_IDS` const array. The wire form is
    /// produced by serde's `rename_all = "snake_case"` rename; we
    /// compare via JSON round-trip so a future rename to the enum
    /// (e.g., adding `#[serde(rename = "...")]` on a variant) is
    /// reflected here without a separate Display impl to maintain.
    #[test]
    fn invariant_ids_strings_match_enum_serde() {
        assert_eq!(
            InvariantId::ALL.len(),
            INVARIANT_IDS.len(),
            "InvariantId::ALL and INVARIANT_IDS disagree on cardinality"
        );
        for (id, expected) in InvariantId::ALL.iter().zip(INVARIANT_IDS.iter()) {
            let json = serde_json::to_string(id).expect("InvariantId serializes to JSON");
            // Strip surrounding quotes from the JSON string literal.
            let computed = json.trim_matches('"');
            assert_eq!(
                &computed, expected,
                "drift between InvariantId::{:?} ({}) and INVARIANT_IDS entry ({})",
                id, computed, expected
            );
        }
    }
}
