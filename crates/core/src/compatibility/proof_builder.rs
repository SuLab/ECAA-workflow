//! Helper for assembling `CompatibilityProof` instances inside the
//! engine. Keeps the engine code focused on decisions rather than
//! struct construction.
//!
//! V3 alignment `ProofBuilder` carries the producer and
//! consumer `SemanticType` references so `with_subsumption_path` can
//! enrich an empty path against the typed source instead of leaving the
//! `ontology_subsumption_path` field empty. The enrichment closes the
//! F6 invariant ("every proof names producer output, consumer input,
//! matched facets, ontology path (or local-extension rationale),
//! validators, adapter insertions, residual assumptions") for the two
//! variants where the engine's `semantic_compat` returns an empty path:
//!
//! - `LocalExtension {id, namespace, proposed_parent_terms,...}` —
//!   path is `proposed_parent_terms ++ ["<namespace>:<id>"]`.
//! - `Opaque {description}` — path is `["opaque:<stable_hash>"]` so
//!   two `Opaque` ports with the same description share a stable
//!   bucket id.

use crate::workflow_contracts::edge::{
    CompatibilityProof, FacetMatch, FacetMatchKind, ProofEvidence,
};
use crate::workflow_contracts::semantic_type::SemanticType;

/// ProofBuilder data.
pub struct ProofBuilder {
    proof: CompatibilityProof,
    producer_type: SemanticType,
    consumer_type: SemanticType,
}

impl ProofBuilder {
    /// New.
    pub fn new(producer: &SemanticType, consumer: &SemanticType) -> Self {
        Self {
            proof: CompatibilityProof {
                producer_type: producer.stable_id(),
                consumer_type: consumer.stable_id(),
                ..Default::default()
            },
            producer_type: producer.clone(),
            consumer_type: consumer.clone(),
        }
    }

    /// Record the ontology subsumption path. F6 enriches an empty
    /// path against the typed producer / consumer
    /// `SemanticType` so the F6 invariant ("every proof carries a
    /// non-empty ontology path or a local-extension rationale") holds
    /// across the `LocalExtension`/`Opaque` variants.
    pub fn with_subsumption_path(mut self, path: Vec<String>) -> Self {
        let path = if path.is_empty() {
            // F6 enrichment fallback: derive the path from the typed
            // semantic-type variants. Producer / consumer
            // `ontology_path_for_semantic_type` outputs are chained,
            // de-duplicated, and recorded so the proof always carries
            // *some* ontological evidence — even when the engine
            // returned an empty match path (same-id LocalExtension
            // self-loops, Opaque pass-throughs).
            let mut enriched: Vec<String> = ontology_path_for_semantic_type(&self.producer_type);
            for term in ontology_path_for_semantic_type(&self.consumer_type) {
                if !enriched.contains(&term) {
                    enriched.push(term);
                }
            }
            enriched
        } else {
            path
        };
        self.proof.ontology_subsumption_path = path;
        self
    }

    /// Add facet.
    pub fn add_facet(
        &mut self,
        facet: &str,
        producer: Option<&str>,
        consumer: Option<&str>,
        kind: FacetMatchKind,
        rationale: Option<String>,
    ) {
        self.proof.facet_matches.push(FacetMatch {
            facet: facet.to_string(),
            producer: producer.unwrap_or("").to_string(),
            consumer: consumer.unwrap_or("").to_string(),
            kind,
            rationale,
        });
    }

    /// Add warning.
    pub fn add_warning(&mut self, w: impl Into<String>) {
        self.proof.warnings.push(w.into());
    }

    /// Add adapter node.
    pub fn add_adapter_node(&mut self, id: impl Into<String>) {
        self.proof.inserted_adapter_node_ids.push(id.into());
    }

    /// Record a policy
    /// decision in the proof. Format:
    /// `<bundle_id>:<check_kind>[:<extra>]`. Surfaces in RO-Crate /
    /// WRROC provenance as the active-policy audit trail.
    pub fn add_policy_decision(&mut self, decision: impl Into<String>) {
        self.proof.policy_decisions.push(decision.into());
    }

    /// Record a supporting evidence entry in the proof. Registry snapshot
    /// ids and validator run outcomes populate this so the `evidence` field
    /// is non-empty for every produced proof, satisfying the grant's
    /// "registry-snapshot ids, validator outputs" claim and enabling
    /// replay determinism tests.
    pub fn add_evidence(&mut self, ev: ProofEvidence) {
        self.proof.evidence.push(ev);
    }

    /// With rationale.
    pub fn with_rationale(mut self, r: impl Into<String>) -> Self {
        self.proof.rationale = Some(r.into());
        self
    }

    /// Build.
    pub fn build(self) -> CompatibilityProof {
        self.proof
    }
}

/// V3 alignment derive an ontology-path slice from a
/// typed `SemanticType`. Used by `with_subsumption_path` to enrich an
/// empty path when the engine's `semantic_compat` returned `Ok(Vec::new())`
/// (same-id `LocalExtension` self-loops, `Opaque` pass-throughs).
///
/// Mapping:
/// - `OntologyTerm { iri,... }` → `[iri]`.
/// - `LocalExtension { namespace, id, proposed_parent_terms,... }` →
///   `proposed_parent_terms ++ ["<namespace>:<id>"]`. The local id is
///   appended last so the F6 path always reaches the actual port's
///   typed identity, regardless of how many parent terms were proposed.
/// - `Opaque { description }` → `["opaque:<stable_hash>"]`. The hash is
///   the first 8 hex chars of the SHA-256 of the description, so two
///   ports with the same description share a stable bucket id.
pub fn ontology_path_for_semantic_type(st: &SemanticType) -> Vec<String> {
    match st {
        SemanticType::OntologyTerm { iri, .. } => vec![iri.clone()],
        SemanticType::LocalExtension {
            namespace,
            id,
            proposed_parent_terms,
            ..
        } => {
            let mut path = proposed_parent_terms.clone();
            path.push(format!("{namespace}:{id}"));
            path
        }
        SemanticType::Opaque { description } => {
            vec![format!("opaque:{}", short_hash(description))]
        }
        SemanticType::Union { members } => {
            // For a union, collect the paths of all members and
            // de-duplicate. The resulting path gives the proof
            // builder a non-empty ontological anchor that covers
            // each member's type identity.
            let mut path: Vec<String> = Vec::new();
            for m in members {
                for term in ontology_path_for_semantic_type(m) {
                    if !path.contains(&term) {
                        path.push(term);
                    }
                }
            }
            if path.is_empty() {
                vec![format!("union:{}", st.stable_id())]
            } else {
                path
            }
        }
    }
}

fn short_hash(s: &str) -> String {
    // 8 hex chars = 4 bytes for compactness without collision pressure
    // at the LocalExtension/Opaque scale we expect.
    crate::hash_utils::sha256_short(s.as_bytes(), 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ontology_path_for_ontology_term_returns_iri() {
        let st = SemanticType::edam("data:0863", "Sequence alignment");
        let path = ontology_path_for_semantic_type(&st);
        assert_eq!(path, vec!["data:0863".to_string()]);
    }

    #[test]
    fn ontology_path_for_local_extension_includes_parents_and_id() {
        use crate::workflow_contracts::semantic_type::LocalExtensionMaturity;
        let st = SemanticType::LocalExtension {
            namespace: "swfc".into(),
            id: "novel_thing".into(),
            proposed_parent_terms: vec!["data:0863".into(), "data:1234".into()],
            definition: "test".into(),
            maturity: LocalExtensionMaturity::Minted,
        };
        let path = ontology_path_for_semantic_type(&st);
        assert_eq!(path.len(), 3);
        assert!(path.contains(&"data:0863".to_string()));
        assert!(path.contains(&"data:1234".to_string()));
        assert!(path.contains(&"swfc:novel_thing".to_string()));
    }

    #[test]
    fn ontology_path_for_opaque_is_stable_bucket() {
        let st1 = SemanticType::Opaque {
            description: "profiler failed: corrupted header".into(),
        };
        let st2 = SemanticType::Opaque {
            description: "profiler failed: corrupted header".into(),
        };
        let st3 = SemanticType::Opaque {
            description: "different reason".into(),
        };
        let p1 = ontology_path_for_semantic_type(&st1);
        let p2 = ontology_path_for_semantic_type(&st2);
        let p3 = ontology_path_for_semantic_type(&st3);
        // Same description -> same bucket id.
        assert_eq!(p1, p2);
        // Different description -> different bucket.
        assert_ne!(p1, p3);
        // Format check.
        assert!(p1[0].starts_with("opaque:"));
        assert_eq!(p1[0].len(), "opaque:".len() + 8);
    }

    #[test]
    fn empty_path_enrichment_via_with_subsumption_path() {
        // Same-id LocalExtension self-loop returns Ok(Vec::new()) from
        // semantic_compat; verify the builder enriches it.
        use crate::workflow_contracts::semantic_type::LocalExtensionMaturity;
        let st = SemanticType::LocalExtension {
            namespace: "lab".into(),
            id: "foo".into(),
            proposed_parent_terms: vec!["data:1234".into()],
            definition: "".into(),
            maturity: LocalExtensionMaturity::Minted,
        };
        let proof = ProofBuilder::new(&st, &st)
            .with_subsumption_path(vec![])
            .build();
        assert!(
            !proof.ontology_subsumption_path.is_empty(),
            "F6: empty path should be enriched from typed producer/consumer"
        );
        // Should include the local id.
        assert!(proof
            .ontology_subsumption_path
            .iter()
            .any(|p| p == "lab:foo"));
    }
}
