//! Facet unification — per-facet match decisions feeding into
//! `CompatibilityProof.facet_matches`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::workflow_contracts::edge::FacetMatchKind;

/// Outcome of unifying one facet across producer/consumer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub enum FacetUnification {
    /// Identical strings.
    Exact,
    /// Producer subtype of consumer (e.g. consumer accepts
    /// "mammal", producer is "Homo sapiens").
    Subtype { rationale: String },
    /// Different but reconcilable via a declared substitution
    /// (e.g. genome build GRCh37 → GRCh38 via UCSC liftover).
    /// The adapter that does the substitution is named in the
    /// rationale; a downstream pass inserts it.
    Substituted {
        /// Adapter id.
        adapter_id: String,
        /// Rationale.
        rationale: String,
    },
    /// One side missing — composer doesn't know whether they
    /// match. Surfaces as Unknown in `CompatibilityResult`.
    Unknown { reason: String },
    /// Hard mismatch with no defensible substitution.
    Incompatible { rationale: String },
}

impl FacetUnification {
    /// Match kind.
    pub fn match_kind(&self) -> FacetMatchKind {
        match self {
            FacetUnification::Exact => FacetMatchKind::Exact,
            FacetUnification::Subtype { .. } => FacetMatchKind::Subtype,
            FacetUnification::Substituted { .. } => FacetMatchKind::Substituted,
            FacetUnification::Unknown { .. } => FacetMatchKind::Unknown,
            // Incompatible is not represented in FacetMatchKind —
            // it's a hard failure that propagates up to
            // `CompatibilityResult::Incompatible` rather than
            // appearing on a successful proof.
            FacetUnification::Incompatible { .. } => FacetMatchKind::Unknown,
        }
    }

    /// Is compatible.
    pub fn is_compatible(&self) -> bool {
        !matches!(self, FacetUnification::Incompatible { .. })
    }
}

/// Unify a single facet. Returns the typed outcome.
///
/// Rules:
///
/// - Both `None` → `Unknown` ("facet unset on both sides").
/// - Producer `Some(x)`, consumer `None` → `Exact` (consumer
///   doesn't constrain).
/// - Producer `None`, consumer `Some(x)` → `Unknown` ("producer
///   didn't declare {facet}").
/// - Both `Some(x)`, equal → `Exact`.
/// - Both `Some(x)`, different → caller decides via `subtype_check`
///   / `substitution_adapter` callbacks.
pub fn unify_facet(
    facet_name: &str,
    producer: Option<&str>,
    consumer: Option<&str>,
    subtype_check: impl FnOnce(&str, &str) -> Option<String>,
    substitution_adapter: impl FnOnce(&str, &str) -> Option<(String, String)>,
) -> FacetUnification {
    match (producer, consumer) {
        (None, None) => FacetUnification::Unknown {
            reason: format!("{facet_name} unset on both producer and consumer"),
        },
        (Some(_), None) => FacetUnification::Exact,
        (None, Some(_)) => FacetUnification::Unknown {
            reason: format!("producer did not declare {facet_name}"),
        },
        (Some(p), Some(c)) if p == c => FacetUnification::Exact,
        (Some(p), Some(c)) => {
            if let Some(rationale) = subtype_check(p, c) {
                FacetUnification::Subtype { rationale }
            } else if let Some((adapter_id, rationale)) = substitution_adapter(p, c) {
                FacetUnification::Substituted {
                    adapter_id,
                    rationale,
                }
            } else {
                FacetUnification::Incompatible {
                    rationale: format!("{facet_name}: producer={p}, consumer={c}"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never_subtype(_: &str, _: &str) -> Option<String> {
        None
    }
    fn never_adapter(_: &str, _: &str) -> Option<(String, String)> {
        None
    }

    #[test]
    fn exact_match_when_equal() {
        let u = unify_facet(
            "genome_build",
            Some("GRCh38"),
            Some("GRCh38"),
            never_subtype,
            never_adapter,
        );
        assert!(matches!(u, FacetUnification::Exact));
        assert!(u.is_compatible());
    }

    #[test]
    fn exact_when_consumer_unconstrained() {
        let u = unify_facet(
            "genome_build",
            Some("GRCh38"),
            None,
            never_subtype,
            never_adapter,
        );
        assert!(matches!(u, FacetUnification::Exact));
    }

    #[test]
    fn unknown_when_producer_missing() {
        let u = unify_facet(
            "genome_build",
            None,
            Some("GRCh38"),
            never_subtype,
            never_adapter,
        );
        assert!(matches!(u, FacetUnification::Unknown { .. }));
    }

    #[test]
    fn unknown_when_both_missing() {
        let u = unify_facet("genome_build", None, None, never_subtype, never_adapter);
        assert!(matches!(u, FacetUnification::Unknown { .. }));
    }

    #[test]
    fn incompatible_without_subtype_or_adapter() {
        let u = unify_facet(
            "genome_build",
            Some("GRCh37"),
            Some("GRCh38"),
            never_subtype,
            never_adapter,
        );
        assert!(matches!(u, FacetUnification::Incompatible { .. }));
        assert!(!u.is_compatible());
    }

    #[test]
    fn subtype_when_callback_says_so() {
        let u = unify_facet(
            "organism",
            Some("Homo sapiens"),
            Some("mammal"),
            |p, c| {
                if p == "Homo sapiens" && c == "mammal" {
                    Some("Homo sapiens is a mammal".into())
                } else {
                    None
                }
            },
            never_adapter,
        );
        assert!(matches!(u, FacetUnification::Subtype { .. }));
    }

    #[test]
    fn substitution_when_adapter_provided() {
        let u = unify_facet(
            "genome_build",
            Some("GRCh37"),
            Some("GRCh38"),
            never_subtype,
            |p, c| {
                if p == "GRCh37" && c == "GRCh38" {
                    Some((
                        "ucsc_liftover".into(),
                        "GRCh37 → GRCh38 via UCSC liftover".into(),
                    ))
                } else {
                    None
                }
            },
        );
        assert!(matches!(u, FacetUnification::Substituted { .. }));
    }

    #[test]
    fn match_kind_translates_correctly() {
        assert_eq!(FacetUnification::Exact.match_kind(), FacetMatchKind::Exact);
        assert_eq!(
            FacetUnification::Subtype {
                rationale: "x".into()
            }
            .match_kind(),
            FacetMatchKind::Subtype
        );
        assert_eq!(
            FacetUnification::Substituted {
                adapter_id: "x".into(),
                rationale: "y".into()
            }
            .match_kind(),
            FacetMatchKind::Substituted
        );
        assert_eq!(
            FacetUnification::Unknown { reason: "x".into() }.match_kind(),
            FacetMatchKind::Unknown
        );
    }
}
