//! Facet unification — per-facet match decisions feeding into
//! `CompatibilityProof.facet_matches`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::workflow_contracts::edge::FacetMatchKind;

/// Outcome of unifying one facet across producer/consumer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[non_exhaustive]
pub enum FacetUnification {
    /// Identical strings. Both sides declared the facet and the values agree —
    /// the only outcome that is a two-sided match.
    Exact,
    /// Producer declared the facet; the consumer placed no constraint on it.
    /// Compatible — an unconstrained consumer cannot be violated — but NOT a
    /// match: nothing was checked against anything. Held separate from
    /// [`FacetUnification::Exact`] so a one-sided declaration can never be
    /// counted, or reported in a proof, as an agreement.
    ProducerOnly {
        /// Why this is one-sided rather than an agreement.
        reason: String,
    },
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
            // A one-sided declaration is reported as `Unknown` — literally
            // "engine could not decide", which is what happened: the consumer
            // stated no constraint, so no agreement was established. The wire
            // enum has no producer-only discriminant; the recorded
            // producer/consumer values (consumer empty) and the rationale keep
            // it distinguishable from a facet neither side declared. What it
            // must NOT be is `Exact`, which asserts a two-sided match.
            FacetUnification::ProducerOnly { .. } => FacetMatchKind::Unknown,
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

    /// Is compatible. `ProducerOnly` is compatible: a consumer that constrains
    /// nothing cannot be violated. Only a hard mismatch is incompatible.
    pub fn is_compatible(&self) -> bool {
        !matches!(self, FacetUnification::Incompatible { .. })
    }

    /// True only for a two-sided agreement. Use this — never
    /// `matches!(.., Exact)` plus a side-channel check on whether the consumer
    /// declared anything — when counting or reporting facet agreement.
    pub fn is_two_sided_match(&self) -> bool {
        matches!(self, FacetUnification::Exact)
    }
}

/// Unify a single facet. Returns the typed outcome.
///
/// Rules:
///
/// - Both `None` → `Unknown` ("facet unset on both sides").
/// - Producer `Some(x)`, consumer `None` → `ProducerOnly` (the consumer
///   doesn't constrain the facet, so the edge holds, but no agreement was
///   checked — this is NOT `Exact`).
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
        (Some(_), None) => FacetUnification::ProducerOnly {
            reason: format!(
                "{facet_name}: producer declared it; consumer does not constrain it \
                 — one-sided declaration, no agreement was checked"
            ),
        },
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

    /// A producer-declared / consumer-unset facet is compatible but is NOT a
    /// two-sided match: `runtime/proofs.jsonl` reported three such rows as
    /// `kind:"exact"` with an empty consumer (and, for `statistical_state`, a
    /// rationale reading "exact statistical-state match on bound port"), which
    /// inflated the both-sides-declared exact count from 23 to 26.
    #[test]
    fn producer_only_is_not_reported_as_a_two_sided_exact() {
        let u = unify_facet(
            "units",
            Some("log2 fold change"),
            None,
            never_subtype,
            never_adapter,
        );

        assert!(
            matches!(u, FacetUnification::ProducerOnly { .. }),
            "one-sided declaration must not unify Exact, got {u:?}"
        );
        assert!(
            !u.is_two_sided_match(),
            "nothing was checked against anything"
        );
        assert_ne!(
            u.match_kind(),
            FacetMatchKind::Exact,
            "the reported kind must differ from a genuine agreement"
        );

        // The verdict is unchanged: an unconstrained consumer cannot be violated.
        assert!(
            u.is_compatible(),
            "a one-sided declaration stays compatible, never Incompatible"
        );

        // And the rationale must not claim a match.
        let FacetUnification::ProducerOnly { reason } = &u else {
            unreachable!("asserted above")
        };
        assert!(
            reason.contains("units") && reason.contains("does not constrain"),
            "the rationale must name the facet and the one-sidedness: {reason}"
        );
        assert!(
            !reason.contains("match"),
            "a one-sided declaration must not be rationalized as a match: {reason}"
        );
    }

    /// Both sides declaring the same value is still — and only — `Exact`.
    #[test]
    fn two_sided_agreement_is_still_exact() {
        let u = unify_facet(
            "statistical_state",
            Some("raw_counts"),
            Some("raw_counts"),
            never_subtype,
            never_adapter,
        );
        assert!(u.is_two_sided_match());
        assert_eq!(u.match_kind(), FacetMatchKind::Exact);
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
        assert_eq!(
            FacetUnification::ProducerOnly { reason: "x".into() }.match_kind(),
            FacetMatchKind::Unknown,
            "no wire discriminant for producer-only; it must not borrow Exact's"
        );
    }
}
