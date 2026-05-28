//! Assumption-ledger projection over the decision log.
//!
//! `AssumptionLedger` is a typed projection over the
//! `decision_log::DecisionRecord` substrate. The single source of
//! truth on disk is `runtime/decisions.jsonl`; the typed ledger is
//! the in-memory view consumers (composer outcomes, UI cards)
//! work against.
//!
//! This module translates between the two:
//!
//! - `to_decision_type(&Assumption) -> DecisionType::AssumptionRecorded`
//! - `to_decision_type_resolved(&Assumption) -> Option<DecisionType::AssumptionResolved>`
//! - `from_decision_records(&[DecisionRecord]) -> AssumptionLedger`
//!
//! The projection is deterministic: assumptions are sorted by id,
//! resolutions overwrite the matching id's resolution field, and
//! the round-trip is information-preserving for the typed fields.

use crate::decision_log::{DecisionRecord, DecisionType};

use super::evidence::{
    Assumption, AssumptionLedger, AssumptionResolution, AssumptionSource, RiskClass,
};

/// Map `Assumption.source` to its decision-log string label.
fn source_label(source: &AssumptionSource) -> &'static str {
    match source {
        AssumptionSource::SmeAccepted { .. } => "sme_accepted",
        AssumptionSource::LlmInferred { .. } => "llm_inferred",
        AssumptionSource::LossyAdapter { .. } => "lossy_adapter",
        AssumptionSource::ProfilerDegraded { .. } => "profiler_degraded",
        AssumptionSource::PolicyException { .. } => "policy_exception",
        AssumptionSource::OntologyMappingUnresolved { .. } => "ontology_mapping_unresolved",
        AssumptionSource::RegistryDefault { .. } => "registry_default",
        AssumptionSource::OntologyAdapterInserted { .. } => "ontology_adapter_inserted",
        AssumptionSource::SeedHeuristic { .. } => "seed_heuristic",
    }
}

/// Map `RiskClass` to its decision-log string label.
fn risk_label(r: RiskClass) -> &'static str {
    match r {
        RiskClass::Negligible => "negligible",
        RiskClass::Low => "low",
        RiskClass::Moderate => "moderate",
        RiskClass::High => "high",
        RiskClass::Clinical => "clinical",
    }
}

/// Reverse mapping for `risk_label`. Unknown labels degrade to
/// `Negligible` rather than panicking — the ledger replay path
/// must survive a malformed log entry.
fn parse_risk(label: &str) -> RiskClass {
    match label {
        "low" => RiskClass::Low,
        "moderate" => RiskClass::Moderate,
        "high" => RiskClass::High,
        "clinical" => RiskClass::Clinical,
        _ => RiskClass::Negligible,
    }
}

/// Reverse mapping for `source_label`. Unknown labels return
/// `LlmInferred` with low confidence — best-effort fallback.
fn parse_source(label: &str) -> AssumptionSource {
    match label {
        "sme_accepted" => AssumptionSource::SmeAccepted {
            rationale: String::new(),
        },
        "lossy_adapter" => AssumptionSource::LossyAdapter {
            adapter_node_id: String::new(),
        },
        "profiler_degraded" => AssumptionSource::ProfilerDegraded {
            reason: String::new(),
        },
        "policy_exception" => AssumptionSource::PolicyException {
            policy_id: String::new(),
        },
        "ontology_mapping_unresolved" => AssumptionSource::OntologyMappingUnresolved {
            type_id: String::new(),
        },
        "registry_default" => AssumptionSource::RegistryDefault {
            atom_id: "".into(),
            default_taken: String::new(),
        },
        "ontology_adapter_inserted" => AssumptionSource::OntologyAdapterInserted {
            from_iri: String::new(),
            to_iri: String::new(),
            reason: String::new(),
        },
        "seed_heuristic" => AssumptionSource::SeedHeuristic {
            strategy: String::new(),
        },
        _ => AssumptionSource::LlmInferred {
            confidence: "unknown".into(),
        },
    }
}

/// Build a `DecisionType::AssumptionRecorded` payload from a
/// typed `Assumption`.
pub fn to_decision_type(a: &Assumption) -> DecisionType {
    DecisionType::AssumptionRecorded {
        id: a.id.clone(),
        statement: a.statement.clone(),
        source: source_label(&a.source).to_string(),
        affects_nodes: a.affects_nodes.clone(),
        risk: risk_label(a.risk).to_string(),
    }
}

/// Build the `DecisionType::AssumptionResolved` payload when
/// the assumption is resolved. Returns `None` for unresolved
/// assumptions (the recorded entry already covers them).
pub fn to_decision_type_resolved(a: &Assumption) -> Option<DecisionType> {
    let resolution_label = match &a.resolution {
        AssumptionResolution::Unresolved => return None,
        AssumptionResolution::Accepted { .. } => "accepted",
        AssumptionResolution::Rejected { .. } => "rejected",
        AssumptionResolution::ResolvedByValidator { .. } => "resolved_by_validator",
        // Contradicted resolutions return to Unresolved on the
        // next planner pass; the dedicated `AssumptionContradicted`
        // decision-type carries the cross-confirmation evidence. We
        // emit "contradicted" here for legacy AssumptionResolved log
        // consumers that haven't migrated to the dedicated event.
        AssumptionResolution::Contradicted { .. } => "contradicted",
        // Waivers carry their own policy_rule_id + credentials
        // through `AssumptionWaived` decision-type events. Emit
        // "waived_with_risk" for the legacy resolved-log path so
        // downstream consumers (audit replay, reproducibility check)
        // see a distinct resolution label.
        AssumptionResolution::WaivedWithRisk { .. } => "waived_with_risk",
    };
    Some(DecisionType::AssumptionResolved {
        id: a.id.clone(),
        resolution: resolution_label.to_string(),
    })
}

/// Project a slice of `DecisionRecord`s into an
/// `AssumptionLedger`. Records out of `AssumptionRecorded` /
/// `AssumptionResolved` types are ignored. Iteration order is
/// stable because input slice order is preserved.
pub fn from_decision_records(records: &[DecisionRecord]) -> AssumptionLedger {
    use std::collections::BTreeMap;
    let mut by_id: BTreeMap<String, Assumption> = BTreeMap::new();
    for rec in records {
        match &rec.decision {
            DecisionType::AssumptionRecorded {
                id,
                statement,
                source,
                affects_nodes,
                risk,
            } => {
                by_id.insert(
                    id.clone(),
                    Assumption {
                        id: id.clone(),
                        statement: statement.clone(),
                        source: parse_source(source),
                        affects_nodes: affects_nodes.clone(),
                        risk: parse_risk(risk),
                        resolution: AssumptionResolution::Unresolved,
                        // v3 P5 — chain_of_custody is not projected
                        // through the legacy decision-log surface; it
                        // only attaches when an emit-time PHI/secret
                        // suppression fires.
                        chain_of_custody: None,
                    },
                );
            }
            DecisionType::AssumptionResolved { id, resolution } => {
                if let Some(a) = by_id.get_mut(id) {
                    a.resolution = match resolution.as_str() {
                        "accepted" => AssumptionResolution::Accepted {
                            rationale: String::new(),
                        },
                        "rejected" => AssumptionResolution::Rejected {
                            rationale: String::new(),
                        },
                        "resolved_by_validator" => AssumptionResolution::ResolvedByValidator {
                            validator_id: String::new(),
                            result_ref: String::new(),
                        },
                        // Contradicted/WaivedWithRisk arrive via
                        // dedicated `AssumptionContradicted` /
                        // `AssumptionWaived` decision types; the
                        // legacy `AssumptionResolved` log entry only
                        // sees the label. Reconstruct the resolution
                        // shape with empty fields here; the dedicated
                        // typed events on later phases carry the
                        // full payload for replay.
                        "contradicted" => AssumptionResolution::Contradicted {
                            prior_confirmation_id: String::new(),
                            conflicting_confirmation_id: String::new(),
                        },
                        "waived_with_risk" => AssumptionResolution::WaivedWithRisk {
                            policy_rule_id: super::policy_rule_id::PolicyRuleId::default(),
                            rationale: String::new(),
                            credentials: Vec::new(),
                        },
                        _ => AssumptionResolution::Unresolved,
                    };
                }
            }
            _ => {}
        }
    }
    AssumptionLedger {
        entries: by_id.into_values().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_log::{DecisionActor, DecisionRecord};

    #[test]
    fn typed_assumption_round_trips_through_decision_log() {
        let a = Assumption {
            id: "a_1".into(),
            statement: "Reads are unstranded".into(),
            source: AssumptionSource::LlmInferred {
                confidence: "low".into(),
            },
            affects_nodes: vec!["quantify_features".into()],
            risk: RiskClass::Moderate,
            resolution: AssumptionResolution::Unresolved,
            chain_of_custody: None,
        };
        let dt = to_decision_type(&a);
        let rec = DecisionRecord::new("s", dt, DecisionActor::Llm, None);
        let ledger = from_decision_records(&[rec]);
        assert_eq!(ledger.entries.len(), 1);
        let back = &ledger.entries[0];
        assert_eq!(back.id, "a_1");
        assert_eq!(back.statement, "Reads are unstranded");
        assert!(matches!(back.source, AssumptionSource::LlmInferred { .. }));
        assert_eq!(back.risk, RiskClass::Moderate);
        assert_eq!(back.affects_nodes, vec!["quantify_features"]);
        assert!(matches!(back.resolution, AssumptionResolution::Unresolved));
    }

    #[test]
    fn resolution_record_overwrites_resolution_field() {
        let a = Assumption {
            id: "a_1".into(),
            statement: "Reads are unstranded".into(),
            source: AssumptionSource::LlmInferred {
                confidence: "low".into(),
            },
            affects_nodes: vec![],
            risk: RiskClass::Moderate,
            resolution: AssumptionResolution::Accepted {
                rationale: "SME confirmed".into(),
            },
            chain_of_custody: None,
        };
        let recorded = DecisionRecord::new("s", to_decision_type(&a), DecisionActor::Llm, None);
        let resolved = DecisionRecord::new(
            "s",
            to_decision_type_resolved(&a).unwrap(),
            DecisionActor::Sme,
            None,
        );
        let ledger = from_decision_records(&[recorded, resolved]);
        assert_eq!(ledger.entries.len(), 1);
        assert!(matches!(
            ledger.entries[0].resolution,
            AssumptionResolution::Accepted { .. }
        ));
    }

    #[test]
    fn unresolved_assumption_has_no_resolution_record() {
        let a = Assumption {
            id: "a_1".into(),
            statement: "x".into(),
            source: AssumptionSource::LlmInferred {
                confidence: "low".into(),
            },
            affects_nodes: vec![],
            risk: RiskClass::Negligible,
            resolution: AssumptionResolution::Unresolved,
            chain_of_custody: None,
        };
        assert!(to_decision_type_resolved(&a).is_none());
    }

    #[test]
    fn projection_ignores_unrelated_decision_types() {
        let other = DecisionRecord::new(
            "s",
            DecisionType::Confirm { summary_hash: None },
            DecisionActor::Sme,
            None,
        );
        let ledger = from_decision_records(&[other]);
        assert!(ledger.entries.is_empty());
    }

    #[test]
    fn projection_is_deterministic() {
        let a = Assumption {
            id: "a_2".into(),
            statement: "GRCh38 assumed".into(),
            source: AssumptionSource::LlmInferred {
                confidence: "high".into(),
            },
            affects_nodes: vec!["align_reads".into()],
            risk: RiskClass::Low,
            resolution: AssumptionResolution::Unresolved,
            chain_of_custody: None,
        };
        let b = Assumption {
            id: "a_1".into(),
            statement: "Unstranded reads".into(),
            source: AssumptionSource::LlmInferred {
                confidence: "low".into(),
            },
            affects_nodes: vec![],
            risk: RiskClass::Moderate,
            resolution: AssumptionResolution::Unresolved,
            chain_of_custody: None,
        };
        let recs = vec![
            DecisionRecord::new("s", to_decision_type(&a), DecisionActor::Llm, None),
            DecisionRecord::new("s", to_decision_type(&b), DecisionActor::Llm, None),
        ];
        // BTreeMap preserves sorted order on `id`, so a_1 should
        // come first regardless of insertion order.
        let ledger = from_decision_records(&recs);
        assert_eq!(ledger.entries[0].id, "a_1");
        assert_eq!(ledger.entries[1].id, "a_2");
    }
}
