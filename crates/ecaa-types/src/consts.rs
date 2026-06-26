//! Canonical const arrays defining ECAA v0.2 closed type-set sizes.
//!
//! Source of truth for cross-doc consistency checks. Spec files MUST
//! reference exactly these names; downstream linters import this module
//! instead of hardcoding string lists.

/// ECAA spec version this implementation emits (`v0.2.md` §9.2).
/// Stamped into every `audit-proof-report.json` as `ecaa_version`.
pub const ECAA_VERSION: &str = "0.2";

/// Minimum reader version required to consume packages this
/// implementation emits (`v0.2.md` §9.2 per-package declaration).
pub const MIN_READER_VERSION: &str = "0.2";

/// The 25 node-type names in canonical form (matches `v0.2.md` §5
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

/// Profiles a PRE-EXECUTION PLAN crate truthfully satisfies (no run actions).
///
/// A plan crate — what `build_metadata` emits before the workflow runs —
/// contains a workflow *definition* (a `ComputationalWorkflow` + its
/// `HowToStep`s) but ZERO executed `CreateAction`s. It therefore honestly
/// conforms only to:
///   - base RO-Crate 1.1 (`ro/crate/1.1`),
///   - the WorkflowHub Workflow RO-Crate profile (`workflow-ro-crate/1.0`),
///     which describes a workflow-definition package, and
///   - the bespoke ECAA v0.2 profile (`ecaa/v0.2`).
///
/// The three WRROC v0.5 run profiles (process / workflow / provenance) all
/// document *executed* runs and require real `CreateAction`s with
/// `instrument`s, so a plan crate cannot truthfully claim them and they are
/// deliberately excluded here. They are added only on finalize/execution
/// (see [`EXECUTED_ADDED_PROFILE_IRIS`]) — never to make a profile "pass" via
/// synthetic graph structure.
pub const PLAN_PROFILE_IRIS: &[&str] = &[
    "https://w3id.org/ro/crate/1.2",
    "https://w3id.org/workflowhub/workflow-ro-crate/1.0",
    "https://w3id.org/ecaa/v0.2",
];

/// WRROC v0.5 run profiles a crate may claim ONLY once it carries real
/// executed `CreateAction`s (the finalize/execution path adds these).
///
/// These three profiles document *executed* processes / workflow runs /
/// full provenance. They are added to a crate's `conformsTo` by the
/// finalize path precisely when retrospective per-output `CreateAction`s have
/// been registered (real `instrument`s of real runs) — never by `build_metadata`
/// on a pre-execution plan crate.
pub const EXECUTED_ADDED_PROFILE_IRIS: &[&str] = &[
    "https://w3id.org/ro/wfrun/process/0.5",
    "https://w3id.org/ro/wfrun/workflow/0.5",
    "https://w3id.org/ro/wfrun/provenance/0.5",
];

/// The 6 normative `conformsTo` profile IRIs of a COMPLETE (executed) crate —
/// the union of the plan-set and the executed-adds.
///
/// This is the full set an executed ECAA package declares once it carries real
/// run actions; it remains the canonical "all profiles" reference for the
/// fixture / runcrate conformance gates (which validate executed packages).
/// A pre-execution plan crate declares only the [`PLAN_PROFILE_IRIS`] subset.
pub const REQUIRED_PROFILE_IRIS: &[&str] = &[
    "https://w3id.org/ro/crate/1.2",
    "https://w3id.org/workflowhub/workflow-ro-crate/1.0",
    "https://w3id.org/ro/wfrun/process/0.5",
    "https://w3id.org/ro/wfrun/workflow/0.5",
    "https://w3id.org/ro/wfrun/provenance/0.5",
    "https://w3id.org/ecaa/v0.2",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invariants::InvariantId;

    #[test]
    fn version_consts_match_spec_v0_2() {
        assert_eq!(ECAA_VERSION, "0.2");
        assert_eq!(MIN_READER_VERSION, "0.2");
    }

    #[test]
    fn closed_set_sizes_match_spec() {
        assert_eq!(NODE_TYPES.len(), 25, "spec: 25 node types");
        assert_eq!(EDGE_PREDICATES.len(), 20, "spec: 20 edge predicates");
        assert_eq!(INVARIANT_IDS.len(), 6, "spec: 6 invariants");
        assert_eq!(SIDECAR_PATHS.len(), 8, "spec: 8 sub-graphs");
        assert_eq!(REQUIRED_PROFILE_IRIS.len(), 6, "spec: 6 profile IRIs");
    }

    /// The plan-set + executed-adds partition the full executed profile set:
    /// `PLAN_PROFILE_IRIS ∪ EXECUTED_ADDED_PROFILE_IRIS == REQUIRED_PROFILE_IRIS`,
    /// the two subsets are disjoint, and `provenance/0.5` (the WRROC run
    /// profile a pre-execution plan crate CANNOT truthfully satisfy) lives in
    /// the executed-adds, never the plan set.
    #[test]
    fn plan_and_executed_profile_iris_partition_required() {
        const PROVENANCE: &str = "https://w3id.org/ro/wfrun/provenance/0.5";

        assert_eq!(PLAN_PROFILE_IRIS.len(), 3, "plan crate claims 3 profiles");
        assert_eq!(
            EXECUTED_ADDED_PROFILE_IRIS.len(),
            3,
            "execution adds 3 WRROC run profiles"
        );

        // Disjoint: no IRI is both a plan profile and an executed-add.
        for iri in PLAN_PROFILE_IRIS {
            assert!(
                !EXECUTED_ADDED_PROFILE_IRIS.contains(iri),
                "{iri} must not be in both plan and executed sets"
            );
        }

        // Union equals the full executed set (order-independent).
        let union: std::collections::BTreeSet<&str> = PLAN_PROFILE_IRIS
            .iter()
            .chain(EXECUTED_ADDED_PROFILE_IRIS.iter())
            .copied()
            .collect();
        let required: std::collections::BTreeSet<&str> =
            REQUIRED_PROFILE_IRIS.iter().copied().collect();
        assert_eq!(
            union, required,
            "plan ∪ executed-adds must equal REQUIRED_PROFILE_IRIS"
        );

        // The provenance run profile is execution-only — a plan crate that
        // claimed it would be claiming a profile it cannot truthfully meet.
        assert!(
            !PLAN_PROFILE_IRIS.contains(&PROVENANCE),
            "plan crate must NOT claim provenance/0.5 (no executed run actions)"
        );
        assert!(
            EXECUTED_ADDED_PROFILE_IRIS.contains(&PROVENANCE),
            "provenance/0.5 is added only on execution"
        );
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
