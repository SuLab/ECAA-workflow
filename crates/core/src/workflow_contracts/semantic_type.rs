//! Open-world type system per design §2.
//!
//! The composer never assumes the universe of biological data shapes
//! is closed. `SemanticType` mirrors the design's three-state shape:
//! a known ontology term, a local extension that points at proposed
//! parents, or a fully opaque description.
//!
//! Today's atoms carry primary EDAM IRIs in `AtomDefinition.edam_data`
//! / `edam_format`; `SemanticType::OntologyTerm` is the typed home
//! for those IRIs once an atom is materialized as a `TaskNode`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// v4 P6 / D4 — graduation lifecycle of a minted `LocalExtension`.
///
/// Five stages, monotonic by design (forward-only, no demotion):
/// `Minted` — first sighting (default for newly-minted entries).
/// `Reused` — observed `usage_count` >= 1 in additional sessions.
/// `GraduationCandidate` — crossed all three thresholds in
/// `GraduationThresholds`; UI surfaces an "Annotate for upstream
/// submission" affordance.
/// `ProposedUpstream` — SME has annotated + submitted upstream.
/// `UpstreamAccepted` — upstream ontology accepted the term; the
/// extension can be deprecated in favor of the canonical IRI.
///
/// `f32 success_rate` carrier forces a manual `Eq` impl elsewhere; the
/// enum itself derives only `PartialEq`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum LocalExtensionMaturity {
    /// First sighting. Default for any newly-minted LocalExtension.
    Minted,
    /// Observed in additional sessions but not yet a graduation
    /// candidate. Counters track the cross-session aggregator's view.
    Reused {
        /// Usage count.
        usage_count: u32,
        /// Unique sessions.
        unique_sessions: u32,
    },
    /// Crossed all three graduation thresholds. Carries the cross-
    /// session counters + the modality's primary ontology to propose
    /// upstream submission to.
    GraduationCandidate {
        /// Usage count.
        usage_count: u32,
        /// Unique sessions.
        unique_sessions: u32,
        /// Success rate.
        success_rate: f32,
        /// Graduation target ontology.
        graduation_target_ontology: String,
        /// ISO-8601 timestamp of the threshold crossing. Set by
        /// `tools::intake` when the aggregator's `check_graduation`
        /// returns `Some(_)`.
        proposed_at: String,
    },
    /// SME annotated + submitted upstream. `submission_ref` is the
    /// upstream tracker id (issue/PR/term-request URL).
    ProposedUpstream {
        /// Submission ref.
        submission_ref: String,
        /// Submitted at.
        submitted_at: String,
    },
    /// Upstream ontology accepted the term; `canonical_iri` is the
    /// upstream IRI to migrate to. Sessions that still cite the
    /// original `LocalExtension` should re-emit with the canonical.
    UpstreamAccepted {
        /// Canonical iri.
        canonical_iri: String,
        /// Canonical label.
        canonical_label: String,
        /// Accepted at.
        accepted_at: String,
    },
}

/// Serde-default helper for backward compatibility: pre-v4-P6 session
/// files don't carry `maturity`; deserialize them as `Minted`.
pub fn default_minted() -> LocalExtensionMaturity {
    LocalExtensionMaturity::Minted
}

/// A semantic type for a port or data product. Open-world: unknown
/// future shapes can enter as `LocalExtension` or `Opaque` rather
/// than crashing the composer.
///
/// Note: derives `PartialEq` but not `Eq` because
/// `LocalExtension::maturity` carries an `f32 success_rate` inside the
/// `GraduationCandidate` variant. Callers that need `Eq` semantics
/// (e.g. `BTreeSet<SemanticType>`) compare via `stable_id()` instead.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SemanticType {
    // (Default impl below — `Opaque` with empty description.)
    /// A versioned ontology term. EDAM is the primary backbone for
    /// bioinformatics; other ontologies (CL, OBI, NCIT) attach via
    /// `OntologyTermRef` on `PortContract.ontology_terms`.
    OntologyTerm {
        /// Compact IRI (e.g. `data:1383`, `format:2572`,
        /// `operation:0292`) or `swfc:<slug>` for in-house extensions
        /// per ADR 0004.
        iri: String,
        /// Human-readable label for the IRI.
        label: String,
        /// Ontology release this term was resolved against. `None`
        /// means unversioned; production atoms should supply a pinned
        /// release.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        ontology_version: Option<String>,
    },
    /// A local extension proposed against an ontology. Carries
    /// proposed parent terms so the compatibility engine can do
    /// partial subsumption matches: a local extension satisfies an
    /// ontology consumer when one of its proposed parents is an
    /// ancestor of the consumer's term.
    LocalExtension {
        /// Authority namespace (`swfc:` for in-house, `lab:foo` for
        /// site-specific extensions).
        namespace: String,
        /// Local id within the namespace.
        id: String,
        /// Ontology terms this extension proposes itself a subtype
        /// of. Used by `SemanticType::is_subsumed_by` for the
        /// open-world subsumption check.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        proposed_parent_terms: Vec<String>,
        /// Free-text definition for human review.
        definition: String,
        /// v4 P6 / D4 — graduation lifecycle stage. Defaults to
        /// `Minted` for backward compat with pre-P6 session files.
        #[serde(default = "default_minted")]
        maturity: LocalExtensionMaturity,
    },
    /// Opaque description. Used when intake has not yet identified a
    /// type for an artifact (e.g. profiler degraded with a recorded
    /// reason). Cannot satisfy any non-`Opaque` consumer; planner
    /// only routes opaque-typed edges into `NovelNodeSpec` outcomes.
    Opaque {
        /// Free-text description of why the type is opaque (e.g.
        /// "profiler failed: corrupted FASTQ header").
        description: String,
    },
    /// A union of acceptable semantic types — used by atoms that
    /// accept any of N upstream artifact shapes. A producer
    /// satisfies a Union consumer when the producer's type is
    /// subsumed by ANY member of the union.
    /// `contextualize_findings_with_literature` accept DE results
    /// (data:0951), peak calls (data:3002), or variant calls (data:3498)
    /// via a single `analysis_findings` input port.
    Union { members: Vec<SemanticType> },
}

impl Default for SemanticType {
    /// Default to `Opaque` with an empty description so port
    /// constructors that haven't set a type yet are well-defined
    /// without claiming a specific ontology term.
    fn default() -> Self {
        Self::Opaque {
            description: String::new(),
        }
    }
}

impl SemanticType {
    /// Stable string key for the variant. Used for byte-stable
    /// scoring tuples in the planner.
    pub fn variant_key(&self) -> &'static str {
        match self {
            SemanticType::OntologyTerm { .. } => "ontology_term",
            SemanticType::LocalExtension { .. } => "local_extension",
            SemanticType::Opaque { .. } => "opaque",
            SemanticType::Union { .. } => "union",
        }
    }

    /// Stable identifier for the type. For ontology terms this is
    /// the IRI; for local extensions `<namespace>:<id>`; for opaque
    /// types a synthetic `opaque:<truncated description>`; for unions
    /// `union(<sorted member stable ids joined by |>)` so the id is
    /// byte-stable regardless of member declaration order.
    pub fn stable_id(&self) -> String {
        match self {
            SemanticType::OntologyTerm { iri, .. } => iri.clone(),
            SemanticType::LocalExtension { namespace, id, .. } => {
                format!("{namespace}:{id}")
            }
            SemanticType::Opaque { description } => {
                let normalized = description.replace([' ', '\n', '\t'], "_");
                // truncate(40) panics on non-char-boundary; use chars().take().
                let truncated: String = normalized.chars().take(40).collect();
                format!("opaque:{truncated}")
            }
            SemanticType::Union { members } => {
                // Sort member stable ids so union(A|B) == union(B|A).
                let mut ids: Vec<String> = members.iter().map(|m| m.stable_id()).collect();
                ids.sort();
                format!("union({})", ids.join("|"))
            }
        }
    }

    /// Returns true when `self` (the producer type) is subsumed by
    /// `consumer` — i.e. the producer's shape satisfies the consumer's
    /// type constraint.
    ///
    /// Subsumption rules:
    /// - `Union` consumer: producer is subsumed when it is subsumed by
    ///   ANY member of the union.
    /// - `Union` producer: the producer is subsumed by a non-union consumer
    ///   only when EVERY member of the union is subsumed by the consumer
    ///   (the producer could supply any member, so all must satisfy).
    ///   Two unions: producer is subsumed by consumer-union when every
    ///   producer member is subsumed by the consumer-union (i.e. each
    ///   producer member is subsumed by SOME consumer member).
    /// - For non-union types, `OntologyTerm` exact-IRI match is used;
    ///   `LocalExtension` matches an `OntologyTerm` consumer when any
    ///   proposed parent equals the consumer IRI; `Opaque` never subsumes
    ///   or is subsumed (open-world; compatibility engine handles it).
    ///
    /// Note: this is a lightweight string-equality check without the
    /// EDAM hierarchy walk that `DeterministicCompatibilityEngine` does.
    /// It is sufficient for the archetype-composer path and the
    /// literature-atom port-routing tests.
    pub fn is_subsumed_by(&self, consumer: &SemanticType) -> bool {
        match consumer {
            // Union consumer: self is subsumed when any member subsumes self.
            SemanticType::Union { members } => members.iter().any(|m| self.is_subsumed_by(m)),
            // Non-union consumer with union producer: every member must be
            // subsumed by the consumer.
            _ => match self {
                SemanticType::Union { members } => {
                    members.iter().all(|m| m.is_subsumed_by(consumer))
                }
                SemanticType::OntologyTerm { iri: pi, .. } => match consumer {
                    SemanticType::OntologyTerm { iri: ci, .. } => pi == ci,
                    SemanticType::LocalExtension {
                        proposed_parent_terms,
                        ..
                    } => proposed_parent_terms.iter().any(|p| p == pi),
                    SemanticType::Opaque { .. } => false,
                    SemanticType::Union { .. } => unreachable!("union arm handled above"),
                },
                SemanticType::LocalExtension {
                    proposed_parent_terms,
                    ..
                } => match consumer {
                    SemanticType::OntologyTerm { iri: ci, .. } => {
                        proposed_parent_terms.iter().any(|p| p == ci)
                    }
                    SemanticType::LocalExtension { id: ci, .. } => {
                        matches!(self, SemanticType::LocalExtension { id: pi, .. } if pi == ci)
                    }
                    SemanticType::Opaque { .. } => false,
                    SemanticType::Union { .. } => unreachable!("union arm handled above"),
                },
                SemanticType::Opaque { .. } => false,
            },
        }
    }

    /// Convenience: build an EDAM ontology term with no version pin.
    /// Used by `TaskNode::from_atom` to lift today's `edam_data` /
    /// `edam_format` strings into `SemanticType::OntologyTerm`.
    pub fn edam(iri: impl Into<String>, label: impl Into<String>) -> Self {
        SemanticType::OntologyTerm {
            iri: iri.into(),
            label: label.into(),
            ontology_version: None,
        }
    }

    /// Convenience: build an opaque type with a recorded reason.
    pub fn opaque(reason: impl Into<String>) -> Self {
        SemanticType::Opaque {
            description: reason.into(),
        }
    }
}

/// Reference to an additional ontology term. Used on
/// `PortContract.ontology_terms` to capture multi-ontology
/// annotations (e.g. a count matrix typed as both an EDAM data class
/// and a CL cell type).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct OntologyTermRef {
    /// Compact IRI.
    pub iri: String,
    /// Optional human label for UI rendering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub label: Option<String>,
    /// Ontology release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub ontology_version: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_ontology_term() {
        let st = SemanticType::edam("data:1383", "Sequence assembly");
        let json = serde_json::to_string(&st).unwrap();
        let back: SemanticType = serde_json::from_str(&json).unwrap();
        assert_eq!(st, back);
        assert_eq!(st.variant_key(), "ontology_term");
        assert_eq!(st.stable_id(), "data:1383");
    }

    #[test]
    fn round_trip_local_extension() {
        let st = SemanticType::LocalExtension {
            namespace: "swfc".into(),
            id: "scrnaseq_doublet_score".into(),
            proposed_parent_terms: vec!["data:2603".into()],
            definition: "Per-cell doublet probability".into(),
            maturity: LocalExtensionMaturity::Minted,
        };
        let json = serde_json::to_string(&st).unwrap();
        let back: SemanticType = serde_json::from_str(&json).unwrap();
        assert_eq!(st, back);
        assert_eq!(st.stable_id(), "swfc:scrnaseq_doublet_score");
    }

    #[test]
    fn pre_p6_serde_default_for_maturity() {
        // Pre-v4-P6 sessions don't carry `maturity`; deserialize as
        // `Minted` so reading older session files still works.
        let json = r#"{"kind":"local_extension","namespace":"swfc","id":"x","proposed_parent_terms":[],"definition":"d"}"#;
        let st: SemanticType = serde_json::from_str(json).unwrap();
        match st {
            SemanticType::LocalExtension { maturity, .. } => {
                assert!(matches!(maturity, LocalExtensionMaturity::Minted));
            }
            other => panic!("expected LocalExtension, got {other:?}"),
        }
    }

    #[test]
    fn round_trip_opaque() {
        let st = SemanticType::opaque("profiler_failed");
        let json = serde_json::to_string(&st).unwrap();
        let back: SemanticType = serde_json::from_str(&json).unwrap();
        assert_eq!(st, back);
        assert_eq!(st.variant_key(), "opaque");
    }

    #[test]
    fn variant_keys_are_stable() {
        assert_eq!(SemanticType::edam("a", "b").variant_key(), "ontology_term");
        assert_eq!(
            SemanticType::LocalExtension {
                namespace: "swfc".into(),
                id: "x".into(),
                proposed_parent_terms: vec![],
                definition: "".into(),
                maturity: LocalExtensionMaturity::Minted,
            }
            .variant_key(),
            "local_extension"
        );
        assert_eq!(SemanticType::opaque("x").variant_key(), "opaque");
        assert_eq!(
            SemanticType::Union {
                members: vec![SemanticType::edam("data:0951", "DE")],
            }
            .variant_key(),
            "union"
        );
    }

    #[test]
    fn round_trip_union() {
        let u = SemanticType::Union {
            members: vec![
                SemanticType::edam("data:0951", "Statistical estimate score"),
                SemanticType::edam("data:3002", "Annotation track"),
            ],
        };
        let json = serde_json::to_string(&u).unwrap();
        let back: SemanticType = serde_json::from_str(&json).unwrap();
        assert_eq!(u, back);
        assert_eq!(u.variant_key(), "union");
    }

    #[test]
    fn union_subsumes_consumer_when_any_member_matches() {
        let u = SemanticType::Union {
            members: vec![
                SemanticType::OntologyTerm {
                    iri: "data:0951".into(),
                    label: "DE".into(),
                    ontology_version: None,
                },
                SemanticType::OntologyTerm {
                    iri: "data:3002".into(),
                    label: "Peak".into(),
                    ontology_version: None,
                },
            ],
        };
        let consumer_de = SemanticType::OntologyTerm {
            iri: "data:0951".into(),
            label: "DE".into(),
            ontology_version: None,
        };
        let consumer_unrelated = SemanticType::OntologyTerm {
            iri: "data:1255".into(),
            label: "X".into(),
            ontology_version: None,
        };
        // A producer of type data:0951 is subsumed by the union (any member matches).
        assert!(
            consumer_de.is_subsumed_by(&u),
            "data:0951 should be subsumed by union containing data:0951"
        );
        // A producer of an unrelated type is not subsumed by the union.
        assert!(
            !consumer_unrelated.is_subsumed_by(&u),
            "data:1255 should not be subsumed by union of data:0951 / data:3002"
        );
    }

    #[test]
    fn union_stable_id_is_byte_stable_across_member_order() {
        let u1 = SemanticType::Union {
            members: vec![
                SemanticType::OntologyTerm {
                    iri: "data:0951".into(),
                    label: "A".into(),
                    ontology_version: None,
                },
                SemanticType::OntologyTerm {
                    iri: "data:3002".into(),
                    label: "B".into(),
                    ontology_version: None,
                },
            ],
        };
        let u2 = SemanticType::Union {
            members: vec![
                SemanticType::OntologyTerm {
                    iri: "data:3002".into(),
                    label: "B".into(),
                    ontology_version: None,
                },
                SemanticType::OntologyTerm {
                    iri: "data:0951".into(),
                    label: "A".into(),
                    ontology_version: None,
                },
            ],
        };
        assert_eq!(
            u1.stable_id(),
            u2.stable_id(),
            "stable_id must sort members so order doesn't matter for byte stability"
        );
        // Verify the format.
        assert!(
            u1.stable_id().starts_with("union("),
            "stable_id format should start with 'union('"
        );
    }

    #[test]
    fn union_yaml_round_trip() {
        // Verify the serde(tag = "kind") shape works for Union in YAML.
        let yaml = r#"
kind: union
members:
  - kind: ontology_term
    iri: "data:0951"
    label: "Statistical estimate score"
    ontology_version: "EDAM-1.25"
  - kind: ontology_term
    iri: "data:3002"
    label: "Annotation track"
    ontology_version: "EDAM-1.25"
"#;
        let st: SemanticType = serde_yml::from_str(yaml).expect("yaml round-trip failed");
        assert_eq!(st.variant_key(), "union");
        match &st {
            SemanticType::Union { members } => {
                assert_eq!(members.len(), 2);
                assert_eq!(members[0].stable_id(), "data:0951");
                assert_eq!(members[1].stable_id(), "data:3002");
            }
            other => panic!("expected Union, got {other:?}"),
        }
    }
}
