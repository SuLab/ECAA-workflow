//! Typed composition errors + their conversion to typed
//! `ComposeOutcome`s.
//!
//! The composer's caller (the conversation crate's LLM tool layer)
//! maps these variants to user-facing blocker variants.
//!
//! The `CompositionError::to_compose_outcome` mapping is the typed
//! bridge from composer-internal failures onto the
//! `crate::workflow_contracts::outcome::ComposeOutcome` shape the
//! conversation crate consumes.

/// Typed composition errors. The composer's caller
/// (initially the conversation crate's LLM tool layer; later the
/// builder's archetype-path branch) maps these to user-facing
/// blocker variants.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompositionError {
    /// No archetype in the registry matched the goal's
    /// (edam_data, edam_format, project_class) triple AND the
    /// backward-chain fallback also failed to find any producer atom.
    #[error("no archetype matches goal (data={target_data}, format={target_format:?}, class={target_class})")]
    NoArchetypeMatch {
        /// Target data.
        target_data: String,
        /// Target format.
        target_format: Option<String>,
        /// Target class.
        target_class: String,
    },
    /// An archetype matched but its scaffold references atom ids
    /// the registry doesn't know. Indicates the archetype catalog
    /// was authored against a future atom set or the atom registry
    /// is incomplete.
    #[error("archetype {archetype_id} references unknown atom {atom_id}")]
    UnknownAtom {
        /// Archetype id.
        archetype_id: String,
        atom_id: crate::ids::AtomId,
    },
    /// Multiple archetypes tied at the top score and the caller has
    /// not surfaced the SME-facing tie-break card yet.
    #[error("{} archetypes tied at score {score} — SME tie-break needed: {}", candidates.len(), candidates.join(", "))]
    TieRequiresSmeDecision { candidates: Vec<String>, score: u32 },
    /// The matched archetype declares an atom whose `excludes`
    /// list contains another atom in the same scaffold (exclusion
    /// consistency check).
    #[error("archetype {archetype_id} composition violates exclusion: {atom_a} excludes {atom_b}")]
    ExclusionConflict {
        /// Archetype id.
        archetype_id: String,
        /// Atom a.
        atom_a: String,
        /// Atom b.
        atom_b: String,
    },
    /// Acyclicity. Topological sort over the
    /// composition's `depends_on` edges discovered a cycle; the
    /// `cycle` field carries the atom ids in cycle order so the
    /// blocker UI can render the trace.
    #[error("composition contains a dependency cycle: {}", cycle.join(" -> "))]
    CycleDetected { cycle: Vec<String> },
    /// Goal reachability. None of the composed atoms'
    /// outputs (`edam_data` + `edam_format`) match the goal's
    /// requested shape. Catches the pathological case where producers
    /// exist but their declared outputs drifted from the goal between
    /// registries.
    #[error("no composed atom produces the goal shape: {goal}")]
    GoalUnreachable { goal: String },
    /// Input satisfiability. An atom in the composition
    /// declared a `depends_on` id that references neither another
    /// atom in the composition nor an intake-supplied input.
    /// Composer caller maps this to a `Blocker::DataShape` variant
    /// surfacing the missing input field to the SME.
    #[error("atom {atom} requires input {missing} which is neither composed nor intake-supplied")]
    InputUnsatisfied { atom: String, missing: String },
    /// Attribute resolution. An atom's
    /// `method_choice.deferred_to` pointer references an id that is
    /// not a Discovery atom in the composition. The composer's
    /// `discover_*` runtime selector requires a sibling Discovery
    /// atom to fan its choice into; otherwise the operation atom
    /// can't pick a method.
    #[error("atom {atom} method_choice deferred to {deferred_to} but no Discovery atom in composition matches")]
    MethodChoiceUnresolved { atom: String, deferred_to: String },
    /// Gate well-formedness. An atom's `excludes:` list
    /// names an id that doesn't exist in the atom registry.
    /// Distinct from `ExclusionConflict` (which fires when the
    /// excluded atom is in the same composition); this one catches
    /// stale atom ids in the gate list at composer time.
    #[error("atom {atom} excludes unknown atom id {excluded}")]
    MalformedExclusion { atom: String, excluded: String },
    /// Slot-fill validation. A required atom in the
    /// composition declared an input slot whose expected EDAM data
    /// class isn't satisfied by an upstream composed atom's output
    /// AND isn't supplied by the SME's intake (per the
    /// `PortMappingRegistry` lookup). Optional atoms with unfilled
    /// slots are silently skipped (`required: false` after slot-
    /// fill); only required atoms surface this error. The
    /// composer's caller (LLM tool layer) maps this to a typed
    /// blocker that names the missing intake field.
    #[error("atom {atom} slot {slot} (expected {expected}) is unfilled — no upstream producer and no intake field supplies a compatible EDAM type")]
    UnfilledRequiredSlot {
        /// Atom.
        atom: String,
        /// Slot.
        slot: String,
        /// Expected.
        expected: String,
    },

    /// Multi-modal joint-source constraint violated.
    /// The atom declared `joint_with: [{lhs, rhs}]` but the
    /// composed producers of `lhs` and `rhs` carry different
    /// `attributes.source_atom` values (or one or both producers
    /// don't carry the attribute at all). Surfaces atom-level
    /// detail so the SME can inspect which upstream pair drifted.
    #[error("atom {atom} requires joint source for ({lhs}, {rhs}) but producers carry diverging attributes.source_atom: {lhs_source:?} vs {rhs_source:?}")]
    JointSourceMismatch {
        /// Atom.
        atom: String,
        /// Lhs.
        lhs: String,
        /// Rhs.
        rhs: String,
        /// Lhs source.
        lhs_source: Option<String>,
        /// Rhs source.
        rhs_source: Option<String>,
    },
    /// `compose:` references an archetype id that the
    /// registry doesn't know. Catches typos + missing-archetype drift
    /// at registry-load time rather than at compose-time.
    #[error("archetype {referenced_from} compose: references unknown archetype {archetype_id}")]
    UnknownInheritedArchetype {
        /// Archetype id.
        archetype_id: String,
        /// Referenced from.
        referenced_from: String,
    },
    /// Cycle detected in the inheritance graph
    /// (A inherits B inherits A). The path field carries the cycle
    /// in walk order for the blocker UI.
    #[error("archetype inheritance cycle: {}", path.join(" -> "))]
    InheritanceCycle { path: Vec<String> },
    /// Inheritance depth exceeded the cap (4). Deep
    /// chains hurt auditability; the cap forces archetype authors
    /// to flatten manually rather than nest.
    #[error("archetype {archetype_id} inheritance depth {depth} exceeds cap {cap} — flatten manually to keep auditability")]
    InheritanceDepthExceeded {
        /// Archetype id.
        archetype_id: String,
        /// Depth.
        depth: usize,
        /// Cap.
        cap: usize,
    },
    /// `replace_atoms: { K: V }` named atom_id `K` that
    /// the inherited archetype doesn't declare. Catches stale
    /// `replace_atoms` entries.
    #[error("archetype {archetype_id} compose: declares replace_atoms target '{target_atom_id}' but inherited archetype {inherited_archetype_id} doesn't include that atom_id")]
    UnknownReplaceTarget {
        /// Archetype id.
        archetype_id: String,
        /// Inherited archetype id.
        inherited_archetype_id: String,
        target_atom_id: crate::ids::AtomId,
    },
    /// `composer_version=4`
    /// (proof-carrying planner) returned an outcome other than
    /// `ValidatedExecutableDag`. Surfaces the typed `ComposeOutcome`
    /// kind + summary so the conversation layer can route to the right
    /// blocker card (PartialDag → CompositionInfeasible; DraftDag →
    /// SME confirmation prompt; NovelNodeSpec → propose_hypothesized_node;
    /// Refusal → policy / safety blocker).
    ///
    /// The full forward / backward / meet-in-the-middle planner
    /// lives in `composer_v4::plan` and produces real `ComposeOutcome`
    /// variants. `composer_version=4` is opt-in via
    /// `SWFC_COMPOSER=semantic` at session creation so v1-v3 users
    /// see no behavior change until their fixtures + persisted
    /// sessions migrate.
    #[error(
        "composer_version=4 returned {outcome_kind} (not executable): {summary}{}",
        format_gaps_suffix(gaps)
    )]
    ComposerV4OutcomeNotExecutable {
        /// Outcome kind.
        outcome_kind: String,
        /// Summary.
        summary: String,
        /// Gaps.
        gaps: Vec<String>,
    },
}

/// Private formatter for `ComposerV4OutcomeNotExecutable::gaps`.
/// Preserves the historical Display output: empty list yields the
/// empty string, non-empty yields ` [gaps: a; b; c]`.
fn format_gaps_suffix(gaps: &[String]) -> String {
    if gaps.is_empty() {
        String::new()
    } else {
        format!(" [gaps: {}]", gaps.join("; "))
    }
}

impl CompositionError {
    /// Map a typed `CompositionError` to a typed `ComposeOutcome`.
    /// `ComposeOutcome` is the unifying wrapper over
    /// `CompositionError`, `BlockerKind::CompositionInfeasible`,
    /// tie-resolution flows, draft/partial DAG paths, novel-node
    /// specs, and refusal reports.
    ///
    /// The mapping is structural — each `CompositionError` variant
    /// gets routed to one of:
    ///
    /// - `ComposeOutcome::PartialDag {... }` for missing-input,
    ///   unfilled-slot, joint-source, goal-unreachable, and unknown-
    ///   producer cases (the composer found *some* of the required
    ///   shape but not all).
    /// - `ComposeOutcome::Refusal {... }` for tie-requires-decision,
    ///   exclusion conflict, cycle detected, malformed exclusion,
    ///   inheritance issues, and v4 stub conditions (hard composition
    ///   blockers that need an SME or operator decision rather than
    ///   more data).
    ///
    /// The empty `WorkflowDag` carrier is intentional — `PartialDag`
    /// is the typed equivalent of "found nothing but here's the gap
    /// report"; the conversation layer can still surface the gaps in
    /// a CompositionInfeasibleCard. A follow-up will thread the
    /// composer's intermediate WorkflowDag (the partial atoms it did
    /// resolve) into the outcome here.
    pub fn to_compose_outcome(&self) -> crate::workflow_contracts::outcome::ComposeOutcome {
        use crate::workflow_contracts::evidence::AssumptionLedger;
        use crate::workflow_contracts::outcome::{ComposeOutcome, GapReport, RefusalReport};
        use crate::workflow_contracts::refusal_kind::RefusalKind;
        use crate::workflow_contracts::task_node::WorkflowDag;
        use crate::workflow_contracts::unblock_path::{ProjectedOutcome, UnblockPath};

        // v4 P4 / F21 — deterministic catch-all unblock path for
        // CompositionError-driven refusals. The richer
        // assumption-aware synthesis lives in
        // `composer_v4::planner::synthesize_unblock_paths`.
        let bioinformatics_lead_escalation = UnblockPath::EscalateToReviewer {
            reviewer_class: "bioinformatics_lead".into(),
            required_artifacts: vec!["refusal_review".into()],
            target_outcome: ProjectedOutcome::DraftDag,
        };

        let empty_dag = WorkflowDag {
            id: "composition_failed".into(),
            nodes: Vec::new(),
            edges: Vec::new(),
            assumptions: AssumptionLedger::default(),
            source_template: None,
        };
        match self {
            CompositionError::NoArchetypeMatch {
                target_data,
                target_format,
                target_class,
            } => ComposeOutcome::PartialDag {
                dag: empty_dag,
                unresolved_gaps: vec![GapReport {
                    id: "no_archetype_match".into(),
                    statement: format!(
                        "No archetype matches goal (data={}, format={:?}, class={})",
                        target_data, target_format, target_class
                    ),
                    missing_port: Some(target_data.clone()),
                    suggestions: vec![
                        "Refine the goal description (intent, modality, project class)".into(),
                        "Import a matching workflow via Phase 10 external registry".into(),
                    ],
                }],
            },
            CompositionError::UnknownAtom {
                archetype_id,
                atom_id,
            } => ComposeOutcome::Refusal {
                report: RefusalReport {
                    id: "unknown_atom".into(),
                    kind: RefusalKind::UncategorizedBlocker,
                    statement: format!(
                        "archetype {archetype_id} references unknown atom {atom_id}; \
                         the atom registry was authored against a different atom set"
                    ),
                    references: vec![archetype_id.clone(), atom_id.to_string()],
                    unblock_paths: vec![bioinformatics_lead_escalation.clone()],
                },
            },
            CompositionError::TieRequiresSmeDecision { candidates, score } => {
                ComposeOutcome::Refusal {
                    report: RefusalReport {
                        id: "tie_requires_sme_decision".into(),
                        kind: RefusalKind::GoalUnderspecified,
                        statement: format!(
                            "{} candidate archetypes tied at score {}",
                            candidates.len(),
                            score
                        ),
                        references: candidates.clone(),
                        unblock_paths: vec![UnblockPath::SupplyMissingMetadata {
                            field: "goal".into(),
                            suggested_value: None,
                            target_outcome: ProjectedOutcome::DraftDag,
                        }],
                    },
                }
            }
            CompositionError::ExclusionConflict {
                archetype_id,
                atom_a,
                atom_b,
            } => ComposeOutcome::Refusal {
                report: RefusalReport {
                    id: "exclusion_conflict".into(),
                    kind: RefusalKind::UncategorizedBlocker,
                    statement: format!(
                        "archetype {archetype_id} has atoms with mutual exclusion: \
                         {atom_a} excludes {atom_b}"
                    ),
                    references: vec![archetype_id.clone(), atom_a.clone(), atom_b.clone()],
                    unblock_paths: vec![bioinformatics_lead_escalation.clone()],
                },
            },
            CompositionError::CycleDetected { cycle } => ComposeOutcome::Refusal {
                report: RefusalReport {
                    id: "cycle_detected".into(),
                    kind: RefusalKind::UncategorizedBlocker,
                    statement: format!("dependency cycle: {}", cycle.join(" -> ")),
                    references: cycle.clone(),
                    unblock_paths: vec![bioinformatics_lead_escalation.clone()],
                },
            },
            CompositionError::GoalUnreachable { goal } => ComposeOutcome::PartialDag {
                dag: empty_dag,
                unresolved_gaps: vec![GapReport {
                    id: "goal_unreachable".into(),
                    statement: format!("composed atoms cannot produce goal {goal}"),
                    missing_port: Some(goal.clone()),
                    suggestions: vec![
                        "Check that registered atom outputs match the goal's edam_data shape"
                            .into(),
                    ],
                }],
            },
            CompositionError::InputUnsatisfied { atom, missing } => ComposeOutcome::PartialDag {
                dag: empty_dag,
                unresolved_gaps: vec![GapReport {
                    id: format!("input_unsatisfied:{atom}:{missing}"),
                    statement: format!(
                        "atom {atom} requires input {missing} but no producer was found"
                    ),
                    missing_port: Some(missing.clone()),
                    suggestions: vec![
                        "Provide the missing input via intake".into(),
                        "Import a producer atom via Phase 10 external registry".into(),
                    ],
                }],
            },
            CompositionError::MethodChoiceUnresolved { atom, deferred_to } => {
                ComposeOutcome::Refusal {
                    report: RefusalReport {
                        id: "method_choice_unresolved".into(),
                        kind: RefusalKind::GoalUnderspecified,
                        statement: format!(
                            "atom {atom} defers method choice to {deferred_to} but no \
                             discovery atom is in scope"
                        ),
                        references: vec![atom.clone(), deferred_to.clone()],
                        unblock_paths: vec![UnblockPath::SupplyMissingMetadata {
                            field: "method".into(),
                            suggested_value: None,
                            target_outcome: ProjectedOutcome::DraftDag,
                        }],
                    },
                }
            }
            CompositionError::MalformedExclusion { atom, excluded } => ComposeOutcome::Refusal {
                report: RefusalReport {
                    id: "malformed_exclusion".into(),
                    kind: RefusalKind::UncategorizedBlocker,
                    statement: format!(
                        "atom {atom} excludes {excluded} but no such atom is registered"
                    ),
                    references: vec![atom.clone(), excluded.clone()],
                    unblock_paths: vec![bioinformatics_lead_escalation.clone()],
                },
            },
            CompositionError::UnfilledRequiredSlot {
                atom,
                slot,
                expected,
            } => ComposeOutcome::PartialDag {
                dag: empty_dag,
                unresolved_gaps: vec![GapReport {
                    id: format!("unfilled_slot:{atom}:{slot}"),
                    statement: format!(
                        "atom {atom} required slot {slot} (expected {expected}) is unfilled"
                    ),
                    missing_port: Some(expected.clone()),
                    suggestions: vec![format!(
                        "Provide intake data of type {expected} for slot {slot}"
                    )],
                }],
            },
            CompositionError::JointSourceMismatch {
                atom,
                lhs,
                rhs,
                lhs_source,
                rhs_source,
            } => ComposeOutcome::Refusal {
                report: RefusalReport {
                    id: "joint_source_mismatch".into(),
                    kind: RefusalKind::UncategorizedBlocker,
                    statement: format!(
                        "atom {atom} requires joint sources for ({lhs}, {rhs}) but \
                         producers carry different source_atom: lhs={:?} vs rhs={:?}",
                        lhs_source, rhs_source
                    ),
                    references: vec![
                        atom.clone(),
                        lhs.clone(),
                        rhs.clone(),
                        lhs_source.clone().unwrap_or_default(),
                        rhs_source.clone().unwrap_or_default(),
                    ],
                    unblock_paths: vec![bioinformatics_lead_escalation.clone()],
                },
            },
            CompositionError::UnknownInheritedArchetype {
                archetype_id,
                referenced_from,
            } => ComposeOutcome::Refusal {
                report: RefusalReport {
                    id: "unknown_inherited_archetype".into(),
                    kind: RefusalKind::UncategorizedBlocker,
                    statement: format!(
                        "archetype {referenced_from} inherits unknown archetype {archetype_id}"
                    ),
                    references: vec![archetype_id.clone(), referenced_from.clone()],
                    unblock_paths: vec![bioinformatics_lead_escalation.clone()],
                },
            },
            CompositionError::InheritanceCycle { path } => ComposeOutcome::Refusal {
                report: RefusalReport {
                    id: "inheritance_cycle".into(),
                    kind: RefusalKind::UncategorizedBlocker,
                    statement: format!("inheritance cycle: {}", path.join(" -> ")),
                    references: path.clone(),
                    unblock_paths: vec![bioinformatics_lead_escalation.clone()],
                },
            },
            CompositionError::InheritanceDepthExceeded {
                archetype_id,
                depth,
                cap,
            } => ComposeOutcome::Refusal {
                report: RefusalReport {
                    id: "inheritance_depth_exceeded".into(),
                    kind: RefusalKind::UncategorizedBlocker,
                    statement: format!(
                        "archetype {archetype_id} inheritance depth {depth} exceeds cap {cap}"
                    ),
                    references: vec![archetype_id.clone()],
                    unblock_paths: vec![bioinformatics_lead_escalation.clone()],
                },
            },
            CompositionError::UnknownReplaceTarget {
                archetype_id,
                inherited_archetype_id,
                target_atom_id,
            } => ComposeOutcome::Refusal {
                report: RefusalReport {
                    id: "unknown_replace_target".into(),
                    kind: RefusalKind::UncategorizedBlocker,
                    statement: format!(
                        "archetype {archetype_id} replace_atoms target '{target_atom_id}' \
                         not in inherited archetype {inherited_archetype_id}"
                    ),
                    references: vec![
                        archetype_id.clone(),
                        inherited_archetype_id.clone(),
                        target_atom_id.to_string(),
                    ],
                    unblock_paths: vec![bioinformatics_lead_escalation.clone()],
                },
            },
            CompositionError::ComposerV4OutcomeNotExecutable {
                outcome_kind,
                summary,
                gaps,
            } => {
                if outcome_kind == "Refusal" {
                    ComposeOutcome::Refusal {
                        report: RefusalReport {
                            id: "v4_refusal".into(),
                            kind: RefusalKind::UncategorizedBlocker,
                            statement: summary.clone(),
                            references: gaps.clone(),
                            unblock_paths: vec![bioinformatics_lead_escalation.clone()],
                        },
                    }
                } else {
                    ComposeOutcome::PartialDag {
                        dag: empty_dag,
                        unresolved_gaps: gaps
                            .iter()
                            .enumerate()
                            .map(|(i, g)| GapReport {
                                id: format!("v4_gap_{}", i),
                                statement: g.clone(),
                                missing_port: None,
                                suggestions: vec![
                                    "no executable DAG; consult ranked alternatives in \
                                     Session::ranked_alternatives"
                                        .into(),
                                ],
                            })
                            .collect(),
                    }
                }
            }
        }
    }
}
