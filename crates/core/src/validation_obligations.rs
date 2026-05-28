//! Validation obligations.
//!
//! `ValidationObligation` (defined in `workflow_contracts::outcome`)
//! names what evidence a node must supply to advance through
//! `LifecycleState`. This module ships the starter library of
//! obligations the design names verbatim (design §18) plus an
//! obligation registry the harness verify endpoint consumes.
//!
//! The harness side wiring (running validators and writing
//! `runtime/validation-reports.jsonl`) lives in `crates/harness`;
//! this module defines the contract the harness implements.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

use crate::workflow_contracts::outcome::ValidationObligation;

/// A bundle of obligations attached to a node. The harness verify
/// endpoint runs the bundle and writes a typed report per
/// obligation.
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
pub struct ValidationBundle {
    /// Stable bundle id (e.g. `bulk_rnaseq_de_v1`).
    pub id: String,
    /// Obligations. Run order is bundle order.
    pub obligations: Vec<ValidationObligation>,
}

/// In-memory registry of validation bundles + the canonical
/// starter obligations.
#[derive(Debug, Clone, Default)]
pub struct ValidationRegistry {
    bundles: BTreeMap<String, ValidationBundle>,
    /// Per-id obligation library — the canonical starter set
    /// design §18 names (p-value range, gene-id-in-annotation,
    /// coordinate-in-contig, train/test leakage, determinism).
    obligations: BTreeMap<String, ValidationObligation>,
}

/// Bundle id for the four renderer-output validation obligations.
/// Runners live in
/// `crates/harness/src/renderer_validators.rs`; obligations are registered
/// in `ValidationRegistry::with_starters()`.
pub const RENDERER_VALIDATION_BUNDLE_ID: &str = "renderer_output_v1";

/// Bundle id for the five literature-atom validation obligations (Task 3 of
/// the literature-atom plan). Runners live in `crates/harness/src/literature_validators.rs`.
pub const LITERATURE_VALIDATION_BUNDLE_ID: &str = "literature_v1";

impl ValidationRegistry {
    /// With starters.
    pub fn with_starters() -> Self {
        let mut reg = Self::default();
        for o in starter_obligations() {
            reg.obligations.insert(o.id.clone(), o);
        }
        for o in renderer_obligations() {
            reg.obligations.insert(o.id.clone(), o);
        }
        reg.register_bundle(renderer_validation_bundle());
        for o in literature_obligations() {
            reg.obligations.insert(o.id.clone(), o);
        }
        reg.register_bundle(literature_validation_bundle());
        reg
    }

    /// Register bundle.
    pub fn register_bundle(&mut self, bundle: ValidationBundle) {
        self.bundles.insert(bundle.id.clone(), bundle);
    }

    /// Register obligation.
    pub fn register_obligation(&mut self, obligation: ValidationObligation) {
        self.obligations.insert(obligation.id.clone(), obligation);
    }

    /// Get bundle.
    pub fn get_bundle(&self, id: &str) -> Option<&ValidationBundle> {
        self.bundles.get(id)
    }

    /// Get obligation.
    pub fn get_obligation(&self, id: &str) -> Option<&ValidationObligation> {
        self.obligations.get(id)
    }

    /// Obligations.
    pub fn obligations(&self) -> impl Iterator<Item = (&String, &ValidationObligation)> {
        self.obligations.iter()
    }
}

fn starter_obligations() -> Vec<ValidationObligation> {
    vec![
        ValidationObligation {
            id: "p_value_in_unit_interval".into(),
            kind: "statistical_sanity".into(),
            statement: "All adjusted p-values must lie in [0, 1]".into(),
            reference: None,
        },
        ValidationObligation {
            id: "gene_id_in_annotation".into(),
            kind: "biological_invariant".into(),
            statement: "Every gene id in output must resolve in the declared annotation".into(),
            reference: None,
        },
        ValidationObligation {
            id: "coordinate_in_contig".into(),
            kind: "biological_invariant".into(),
            statement: "Every genomic coordinate must lie within a contig of the declared assembly"
                .into(),
            reference: None,
        },
        ValidationObligation {
            id: "barcode_matrix_dim_consistency".into(),
            kind: "biological_invariant".into(),
            statement: "Cell barcode count must match count-matrix row/column count".into(),
            reference: None,
        },
        ValidationObligation {
            id: "no_train_test_leakage".into(),
            kind: "metamorphic_test".into(),
            statement: "No row identifier appears in both train and test splits".into(),
            reference: None,
        },
        ValidationObligation {
            id: "deterministic_or_bounded_variance".into(),
            kind: "reproducibility".into(),
            statement: "Output is byte-identical across runs OR variance is within declared bounds"
                .into(),
            reference: None,
        },
        ValidationObligation {
            id: "container_digest_pinned".into(),
            kind: "reproducibility".into(),
            statement: "Implementation container has a sha256 digest pinned at emit time".into(),
            reference: None,
        },
        ValidationObligation {
            id: "no_network_access".into(),
            kind: "security".into(),
            statement: "Task did not attempt network access".into(),
            reference: None,
        },
        ValidationObligation {
            id: "resource_within_declared_bounds".into(),
            kind: "resource_bound".into(),
            statement: "Peak memory and runtime fall within declared resource_class".into(),
            reference: None,
        },
    ]
}

/// Renderer-output obligations. Each maps to one `ValidatorRunner` in
/// `crates/harness/src/renderer_validators.rs`.
fn renderer_obligations() -> Vec<ValidationObligation> {
    vec![
        ValidationObligation {
            id: "renderer_contrast_wcag".into(),
            kind: "accessibility".into(),
            statement: "Figure WCAG contrast ratio must be ≥ 4.5 (AA) as measured by \
                        lib/plotting/quality_scorer.py"
                .into(),
            reference: Some("lib/plotting/quality_scorer.py".into()),
        },
        ValidationObligation {
            id: "renderer_dpi_300".into(),
            kind: "publication_quality".into(),
            statement: "Output PNG pHYs chunk must declare exactly 300 DPI".into(),
            reference: None,
        },
        ValidationObligation {
            id: "renderer_theme_parity".into(),
            kind: "theme_conformance".into(),
            statement: "All colors in the rendered PNG must be present in the theme.json \
                        Wong/Glasbey palette"
                .into(),
            reference: Some("lib/plotting/tests/lint_theme_parity.py".into()),
        },
        ValidationObligation {
            id: "renderer_determinism".into(),
            kind: "reproducibility".into(),
            statement: "Two independent renders of the same figure with the same input data \
                        must produce byte-identical PNG output (Phase 14 deferred — \
                        returns Unimplemented until sandbox execution lands)"
                .into(),
            reference: None,
        },
    ]
}

/// Construct the `ValidationBundle` that groups the four renderer-output
/// obligations. Registered by `ValidationRegistry::with_starters()`.
pub fn renderer_validation_bundle() -> ValidationBundle {
    ValidationBundle {
        id: RENDERER_VALIDATION_BUNDLE_ID.into(),
        obligations: renderer_obligations(),
    }
}

/// Phase literature-atom obligations. Each maps to one
/// `ValidatorRunner` in `crates/harness/src/literature_validators.rs`.
fn literature_obligations() -> Vec<ValidationObligation> {
    vec![
        ValidationObligation {
            id: "pmid_resolves".into(),
            kind: "literature_integrity".into(),
            statement: "Every distinct PMID in a literature claims matrix has a corresponding \
                        evidence/<pmid>.{xml,abstract.json} in the package; PMID is well-formed \
                        (7-9 digit integer); evidence file metadata PMID matches."
                .into(),
            reference: Some("prior_claims_matrix.csv|claims_evidence_matrix.csv".into()),
        },
        ValidationObligation {
            id: "evidence_quote_substring_match".into(),
            kind: "literature_integrity".into(),
            statement: "For each row, evidence_quote is a substring of the retrieved source \
                        after collapse_whitespace_lowercase_v1 normalization."
                .into(),
            reference: Some("prior_claims_matrix.csv|claims_evidence_matrix.csv".into()),
        },
        ValidationObligation {
            id: "redistributable_or_marked".into(),
            kind: "literature_integrity".into(),
            statement: "source_kind == external_pdf_local_only implies redistributable: false; \
                        redistributable: true implies source_kind in {pmc_oa_full_text, abstract_only}."
                .into(),
            reference: Some("prior_claims_matrix.csv|claims_evidence_matrix.csv".into()),
        },
        ValidationObligation {
            id: "claim_row_has_finding_id".into(),
            kind: "literature_integrity".into(),
            statement: "Each claims_evidence_matrix.csv row's finding_id resolves to a \
                        primary-key row in the upstream findings table."
                .into(),
            reference: Some("claims_evidence_matrix.csv".into()),
        },
        ValidationObligation {
            id: "concordance_flag_in_closed_set".into(),
            kind: "literature_integrity".into(),
            statement: "concordance_flag is in \
                        {same_direction, opposite_direction, no_prior_finding, unverifiable}."
                .into(),
            reference: Some("claims_evidence_matrix.csv".into()),
        },
    ]
}

/// Construct the `ValidationBundle` that groups the five literature-atom
/// obligations. Registered by `ValidationRegistry::with_starters()`.
pub fn literature_validation_bundle() -> ValidationBundle {
    ValidationBundle {
        id: LITERATURE_VALIDATION_BUNDLE_ID.into(),
        obligations: literature_obligations(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_registry_has_all_design_obligations() {
        let reg = ValidationRegistry::with_starters();
        let ids: Vec<&str> = reg.obligations().map(|(k, _)| k.as_str()).collect();
        for required in [
            "p_value_in_unit_interval",
            "gene_id_in_annotation",
            "coordinate_in_contig",
            "barcode_matrix_dim_consistency",
            "no_train_test_leakage",
            "deterministic_or_bounded_variance",
        ] {
            assert!(ids.contains(&required), "missing {required}");
        }
    }

    #[test]
    fn starter_obligations_have_distinct_ids() {
        let reg = ValidationRegistry::with_starters();
        let mut ids: Vec<&str> = reg.obligations().map(|(k, _)| k.as_str()).collect();
        ids.sort();
        let mut deduped = ids.clone();
        deduped.dedup();
        assert_eq!(ids, deduped);
    }

    #[test]
    fn bundle_round_trip() {
        let bundle = ValidationBundle {
            id: "bulk_rnaseq_de_v1".into(),
            obligations: vec![ValidationObligation {
                id: "p_value_in_unit_interval".into(),
                kind: "statistical_sanity".into(),
                statement: "All adjusted p-values must lie in [0, 1]".into(),
                reference: None,
            }],
        };
        let json = serde_json::to_string(&bundle).unwrap();
        let back: ValidationBundle = serde_json::from_str(&json).unwrap();
        assert_eq!(bundle, back);
    }

    #[test]
    fn registry_register_and_lookup_bundle() {
        let mut reg = ValidationRegistry::with_starters();
        let bundle = ValidationBundle {
            id: "bulk_rnaseq_de_v1".into(),
            obligations: vec![],
        };
        reg.register_bundle(bundle.clone());
        assert_eq!(reg.get_bundle("bulk_rnaseq_de_v1"), Some(&bundle));
    }

    #[test]
    fn registry_lookup_missing_returns_none() {
        let reg = ValidationRegistry::with_starters();
        assert!(reg.get_bundle("nonexistent").is_none());
    }

    #[test]
    fn registry_includes_literature_obligations() {
        let reg = ValidationRegistry::with_starters();
        assert!(reg.get_obligation("pmid_resolves").is_some());
        assert!(reg
            .get_obligation("evidence_quote_substring_match")
            .is_some());
        assert!(reg.get_obligation("redistributable_or_marked").is_some());
        assert!(reg.get_obligation("claim_row_has_finding_id").is_some());
        assert!(reg
            .get_obligation("concordance_flag_in_closed_set")
            .is_some());
    }

    #[test]
    fn literature_validation_bundle_is_registered() {
        let reg = ValidationRegistry::with_starters();
        let bundle = reg
            .get_bundle(LITERATURE_VALIDATION_BUNDLE_ID)
            .expect("literature bundle must be registered");
        let ids: Vec<&str> = bundle.obligations.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "pmid_resolves",
                "evidence_quote_substring_match",
                "redistributable_or_marked",
                "claim_row_has_finding_id",
                "concordance_flag_in_closed_set",
            ]
        );
    }
}
