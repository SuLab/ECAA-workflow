//! Composer adversarial test corpus (scaffold).
//!
//! Per [DEC Q1.2] and the round-1 research validation, the composer's
//! v1 invariants are exercised by **table-driven** adversarial tests
//! rather than property-test fuzzing. Target: ~200 hand-authored
//! cases covering the failure-mode taxonomy.
//!
//! This file is the scaffold + a seed batch (one representative case
//! per category). Adding a case is editing this file in two places:
//! (a) the `Category` enum gains a variant only when a new failure
//! mode lands — today's seven map to the seven `CompositionError`
//! shapes the composer can return; (b) the `CASES` slice gains rows.
//!
//! Cases that don't need real atom-registry / archetype-registry
//! state synthesise the inputs directly; cases that exercise the
//! real fixture corpus go through `config_root()` (mirrors the
//! `composer_offline.rs` pattern).
//!
//! ## Coverage tracker (extend per S7.14 progress)
//!
//! | Category | Seeded | Target | Gap |
//! |----------|--------|--------|-----|
//! | NoArchetypeMatch | 4 | 25 | 21 |
//! | UnknownAtom | 2 | 10 | 8 |
//! | TieRequiresSmeDecision | 1 | 10 | 9 |
//! | ExclusionConflict | 5 | 25 | 20 |
//! | CycleDetected | 5 | 15 | 10 |
//! | GoalUnreachable | 1 | 25 | 24 |
//! | InputUnsatisfied | 1 | 25 | 24 |
//! | MethodChoiceUnresolved | 4 | 15 | 11 |
//! | MalformedExclusion | 3 | 10 | 7 |
//! | UnfilledRequiredSlot | 0 | 15 | 15 |
//! | JointSourceMismatch | 4 | 10 | 6 |
//! | Per-sample fan-out edges | 0 | 5 | 5 |
//! | Sensitivity-comparison edges | 0 | 10 | 10 |
//! | Succeeds (happy-path regression) | 7 | 20 | 13 |
//! | **Total seeded** | **37** | **220** | **183** |
//!
//! Each follow-up PR that lands a new case batch updates the
//! coverage tracker rows above. The `seed_batch_provides_at_least_one_case_per_seeded_category`
//! test gates that the seed batch covers every category whose row is
//! marked Seeded > 0.
//!
//! Discipline:
//! - Each case is named with a verbatim regex-friendly id so the
//! `cargo test` filter `--exact` lookup works.
//! - Each case asserts the `CompositionError` variant the composer
//! should produce; the body of the case carries the synthesised
//! `(GoalSpec, project_class, AtomRegistry, ArchetypeRegistry)` tuple.
//! - When the gold is "compose succeeds" (validate happy paths), the
//! `expected` field is `Ok` and the assertion confirms the
//! `CompositionResult.atoms` shape.
//! - No live network reads; no LLM calls; no clock reads. Standard
//! composer-determinism contract applies (S7.15).

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom::{AtomAssignee, AtomDefinition, AtomRole};
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::composer::{self, compose_with_version, CompositionError};
use ecaa_workflow_core::goal_spec::GoalSpec;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Failure-mode taxonomy. One variant per shape of `CompositionError`
/// the composer can produce, plus `Succeeds` for happy-path cases the
/// future seed batches will add. Coverage tracker (above) is keyed by
/// these variants — the `dead_code` allow keeps unused future-batch
/// variants compiling without forcing every batch to add a row in the
/// same PR.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Category {
    Succeeds,
    NoArchetypeMatch,
    UnknownAtom,
    TieRequiresSmeDecision,
    ExclusionConflict,
    CycleDetected,
    GoalUnreachable,
    InputUnsatisfied,
    MethodChoiceUnresolved,
    MalformedExclusion,
    UnfilledRequiredSlot,
    JointSourceMismatch,
}

/// A single adversarial-test row. The `description` is the
/// human-readable name (matches the `Coverage tracker` row); the
/// runner is a closure that builds the input tuple and the expected
/// outcome.
struct AdversarialCase {
    id: &'static str,
    category: Category,
    /// Human-readable summary shown when a case fails. Read by
    /// `every_case_runs_to_expected_outcome` via the failure-message
    /// concatenation; explicit `#[allow(dead_code)]` so a case author
    /// can't strip it accidentally during a refactor and lose the
    /// "what was this case actually testing" trail in the panic.
    #[allow(dead_code)]
    description: &'static str,
    /// Build inputs + run the composer; return the actual outcome.
    /// The runner shape lets each case decide how to synthesise the
    /// composer state without a single shape that has to fit every
    /// failure mode.
    runner: fn() -> Outcome,
}

/// Outcome the case asserts against. Mirrors `Result<_, CompositionError>`
/// but extracts only the variant tag (we don't compare atoms-by-atoms
/// for failure cases since the composer's exact error payload is
/// already covered by per-error `compose_*` unit tests). `Ok` is unused
/// in the seed batch but documents the structural option for the
/// happy-path cases future batches add (per Coverage tracker).
#[allow(dead_code)]
enum Outcome {
    Ok { atom_count: usize },
    Err(CompositionError),
}

fn config_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .join("config")
}

/// Build a single `AtomDefinition` with the typical defaults the
/// adversarial cases need. Mirrors the private `make_atom` in
/// `composer.rs`'s test module so the integration cases below can
/// synthesise registries without going through the real fixture corpus.
fn synth_atom(
    id: &str,
    role: AtomRole,
    edam_data: Option<&str>,
    edam_format: Option<&str>,
    depends_on: Vec<&str>,
) -> AtomDefinition {
    AtomDefinition {
        id: id.into(),
        version: "1.0.0".into(),
        role,
        discovery_kind: if matches!(role, AtomRole::Discovery) {
            Some("method".into())
        } else {
            None
        },
        description: format!("synth atom {}", id),
        edam_operation: "operation:0004".into(),
        edam_data: edam_data.map(|s| s.into()),
        edam_format: edam_format.map(|s| s.into()),
        assignee: AtomAssignee::Agent,
        depends_on: depends_on.into_iter().map(|s| s.into()).collect(),
        excludes: vec![],
        attributes: BTreeMap::new(),
        joint_with: vec![],
        inputs: vec![],
        outputs: vec![],
        method_choice: None,
        resource_profile: None,
        preferred_container: None,
        claim_boundary: None,
        iterate: None,
        condition: None,
        required_figures: vec![],
        plot_stage_id: None,
        figure_exempt: None,
        expected_artifacts: vec![],
        required_artifacts: vec![],
        validators: vec![],
        runtime_packages: Default::default(),
        safety: Default::default(),
    }
}

/// Persist a vec of synthesised atoms into a tempdir + load via the
/// public registry path. Mirrors `composer.rs::tests::registry_from`.
fn synth_registry(atoms: Vec<AtomDefinition>) -> AtomRegistry {
    let tmp = tempfile::tempdir().expect("tempdir");
    for atom in &atoms {
        let path = tmp.path().join(format!("{}.yaml", atom.id));
        let yaml = serde_yml::to_string(atom).expect("serialise atom");
        std::fs::write(&path, yaml).expect("write atom yaml");
    }
    // Tempdir leaks; AtomRegistry holds no file handles, so the
    // tempdir cleanup on Drop is fine — but we Box::leak the
    // TempDir guard so the path stays alive for the registry's own
    // bookkeeping (paths-from-yaml). Test-only code; not load-bearing
    // outside this corpus.
    let leaked = Box::leak(Box::new(tmp));
    AtomRegistry::load_from_dir(leaked.path()).expect("load synth atoms")
}

fn empty_archetypes() -> ArchetypeRegistry {
    ArchetypeRegistry::default()
}

/// Synth-archetype helper. Persists a vec of
/// `ArchetypeDefinition` values to a tempdir + loads via the public
/// registry path. Mirrors `synth_registry` for the atom side. Cases
/// that need an archetype-path failure (UnknownAtom,
/// UnfilledRequiredSlot, InputUnsatisfied, archetype-level
/// ExclusionConflict) go through this helper.
fn synth_archetypes(
    archetypes: Vec<ecaa_workflow_core::archetype::ArchetypeDefinition>,
) -> ArchetypeRegistry {
    let tmp = tempfile::tempdir().expect("tempdir");
    for arch in &archetypes {
        let path = tmp.path().join(format!("{}.yaml", arch.id));
        let yaml = serde_yml::to_string(arch).expect("serialise archetype");
        std::fs::write(&path, yaml).expect("write archetype yaml");
    }
    let leaked = Box::leak(Box::new(tmp));
    ArchetypeRegistry::load_from_dir(leaked.path()).expect("load synth archetypes")
}

/// Build a minimal `ArchetypeDefinition` with the defaults the
/// adversarial cases need. The synth atoms ride along on the archetype
/// fast-path so failures route through `archetype_reg.find_match`
/// instead of `backward_chain_compose`.
fn synth_archetype(
    id: &str,
    goal_data: &str,
    goal_format: Option<&str>,
    project_class: &str,
    atoms: Vec<&str>,
) -> ecaa_workflow_core::archetype::ArchetypeDefinition {
    use ecaa_workflow_core::archetype::{
        ArchetypeAtomRef, ArchetypeDefinition, CURRENT_ARCHETYPE_SCHEMA_VERSION,
    };
    ArchetypeDefinition {
        schema_version: CURRENT_ARCHETYPE_SCHEMA_VERSION.into(),
        id: id.into(),
        version: "1.0.0".into(),
        description: format!("synth archetype {}", id),
        sme_summary: format!("synth archetype {}", id),
        goal_data: goal_data.into(),
        goal_format: goal_format.map(|s| s.into()),
        atoms: atoms
            .into_iter()
            .map(|aid| ArchetypeAtomRef {
                atom_id: aid.into(),
                alias: None,
                depends_on: vec![],
                required: true,
                required_figures: None,
                plot_stage_id: None,
                figure_exempt: None,
                expected_artifacts: None,
                required_artifacts: None,
            })
            .collect(),
        slot_mappings: BTreeMap::new(),
        compose: vec![],
        slots: None,
        cross_dependencies: vec![],
        claim_boundary: None,
        project_class: project_class.into(),
        modality_hint: None,
        goal_kind_hint: None,
        preferred_container: None,
        runtime_baseline: Default::default(),
        cross_omics_modalities: vec![],
    }
}

fn de_table_goal() -> GoalSpec {
    GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers: BTreeMap::new(),
        source_prose: None,
        confidence: 0.9,
    }
}

/// The seed batch + first adversarial expansion. Mix of real-catalogue
/// cases (the original three) and synthesised backward-chain cases
/// that target a single failure-mode each. The synth cases bypass the
/// archetype path so each row exercises exactly one validation step
/// (`validate_composition` rule 1-7); the real-catalogue cases cover
/// the archetype matching layer.
///
/// Extend per the coverage tracker. A new case lands as one row in
/// this slice; the runner is responsible for synthesising the input
/// state so the assertion in `every_case_runs_to_expected_outcome`
/// stays single-line.
const CASES: &[AdversarialCase] = &[
    AdversarialCase {
        id: "no-archetype/bogus-project-class",
        category: Category::NoArchetypeMatch,
        description:
            "Real EDAM goal but non-bioinformatics project_class string → NoArchetypeMatch \
             (project_class is part of the matching key). \
             Pinned to v2 (archetype fast-path) — v2 returns NoArchetypeMatch when no \
             archetype matches; v4's proof-carrying planner returns PartialDag for the \
             same input because it attempts gap-closure beyond the archetype fast-path. \
             This case documents the v2 contract; see the composer.rs pin at line ~3130.",
        runner: || -> Outcome {
            let config = config_root();
            let atoms =
                AtomRegistry::load_from_dir(&config.join("stage-atoms")).expect("load atoms");
            let archetypes = ArchetypeRegistry::load_from_dir(&config.join("archetypes"))
                .expect("load archetypes");
            let goal = GoalSpec {
                edam_data: "data:99999".into(),
                edam_format: None,
                modifiers: BTreeMap::new(),
                source_prose: None,
                confidence: 0.5,
            };
            // Pin to v2: the v2 archetype fast-path returns NoArchetypeMatch for a
            // bogus project_class. The default compose() now routes to v4, which
            // returns ComposerV4OutcomeNotExecutable { PartialDag } for this input.
            // Mirrors the pin in composer.rs internal tests (~line 3130).
            match compose_with_version(&goal, "this_class_does_not_exist", &atoms, &archetypes, 2) {
                Ok(r) => panic!(
                    "bogus-class case should fail; got Ok with {} atoms",
                    r.atoms.len()
                ),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    AdversarialCase {
        id: "no-archetype/all-mismatched-fields",
        category: Category::NoArchetypeMatch,
        description:
            "Goal whose (edam_data, edam_format, project_class) triple all miss the catalog: \
             unknown EDAM class with no format under a fictitious project class. Score evaluates \
             to 0 for every archetype → NoArchetypeMatch. \
             Pinned to v2 (archetype fast-path) — v4's proof-carrying planner returns \
             PartialDag for the same input because it attempts gap-closure beyond archetypes.",
        runner: || -> Outcome {
            let config = config_root();
            let atoms =
                AtomRegistry::load_from_dir(&config.join("stage-atoms")).expect("load atoms");
            let archetypes = ArchetypeRegistry::load_from_dir(&config.join("archetypes"))
                .expect("load archetypes");
            let goal = GoalSpec {
                edam_data: "data:ecaax:adversarial-no-such-class".into(),
                edam_format: None,
                modifiers: BTreeMap::new(),
                source_prose: None,
                confidence: 0.3,
            };
            // Pin to v2: v2 archetype fast-path returns NoArchetypeMatch when score=0
            // for every archetype. v4 returns ComposerV4OutcomeNotExecutable { PartialDag }.
            match compose_with_version(&goal, "no_such_class", &atoms, &archetypes, 2) {
                Ok(r) => panic!(
                    "all-mismatched case should fail; got Ok with {} atoms",
                    r.atoms.len()
                ),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    AdversarialCase {
        id: "tie/de-table-multiple-archetypes",
        category: Category::TieRequiresSmeDecision,
        description: "Goal targets data:0951 + format:3475 (DE table). Under v2 (archetype \
             fast-path), multiple archetypes (bulk_rnaseq_de, long_read_rnaseq, \
             metagenomics_taxonomic, clinical_trial_analysis, time_series_forecast) tied at \
             the top score → TieRequiresSmeDecision. Under v4 (proof-carrying planner), the \
             planner resolves the tie and returns Ok. \
             The every_case_runs_to_expected_outcome matcher already handles Ok from a \
             TieRequiresSmeDecision case (dormant-tie branch at line ~1628); this runner \
             propagates both outcomes to the framework rather than panicking on Ok.",
        runner: || -> Outcome {
            let config = config_root();
            let atoms =
                AtomRegistry::load_from_dir(&config.join("stage-atoms")).expect("load atoms");
            let archetypes = ArchetypeRegistry::load_from_dir(&config.join("archetypes"))
                .expect("load archetypes");
            let goal = GoalSpec {
                edam_data: "data:0951".into(),
                edam_format: Some("format:3475".into()),
                modifiers: BTreeMap::new(),
                source_prose: Some("Adversarial: archetype-tie surface".into()),
                confidence: 0.9,
            };
            // Note: v4's proof-carrying planner resolves archetype ties that v2 surfaced
            // as TieRequiresSmeDecision. Both Ok (v4) and Err(TieRequiresSmeDecision) (v2)
            // are accepted by every_case_runs_to_expected_outcome's dormant-tie branch.
            match composer::compose(&goal, "bioinformatics", &atoms, &archetypes) {
                Ok(r) => Outcome::Ok {
                    atom_count: r.atoms.len(),
                },
                Err(e) => Outcome::Err(e),
            }
        },
    },
    // ───────────────── CycleDetected (3) ─────────────────
    // Cycle detection is exercised on the v3 (backward-chain) path.
    // composer::compose() now routes to v4, which returns PartialDag for
    // cycle-containing atom sets (the v4 typed-port search finds no
    // producers when atoms lack port metadata). These cases pin to v3 so
    // the Kahn/walk-dep cycle detection algorithm is exercised directly.
    AdversarialCase {
        id: "cycle/two-node-symmetric",
        category: Category::CycleDetected,
        description: "Two atoms each declare the other as `depends_on`; both produce the goal \
             EDAM. Backward-chain pulls both in; Kahn's algorithm in detect_cycle \
             finds the unbreakable indegree → CycleDetected. Pinned to v3 so the \
             Kahn cycle-detection algorithm is exercised (v4 returns PartialDag for \
             atoms with no typed output ports).",
        runner: || -> Outcome {
            let a = synth_atom(
                "cycle_a",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec!["cycle_b"],
            );
            let b = synth_atom(
                "cycle_b",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec!["cycle_a"],
            );
            let atoms = synth_registry(vec![a, b]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                3,
            ) {
                Ok(r) => panic!("cycle should fail; got Ok with {} atoms", r.atoms.len()),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    AdversarialCase {
        id: "cycle/three-node-ring",
        category: Category::CycleDetected,
        description: "Three atoms a → b → c → a. Only `a` produces the goal EDAM, but \
             walk_dependencies pulls in the whole ring through transitive deps; \
             validate_composition catches it. Pinned to v3 (see cycle/two-node-symmetric).",
        runner: || -> Outcome {
            let a = synth_atom(
                "ring_a",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec!["ring_b"],
            );
            let b = synth_atom("ring_b", AtomRole::Operation, None, None, vec!["ring_c"]);
            let c = synth_atom("ring_c", AtomRole::Operation, None, None, vec!["ring_a"]);
            let atoms = synth_registry(vec![a, b, c]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                3,
            ) {
                Ok(r) => panic!("ring should fail; got Ok with {} atoms", r.atoms.len()),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    AdversarialCase {
        id: "cycle/exceeds-walk-depth",
        category: Category::CycleDetected,
        description: "Mutual-recursion depth exceeds MAX_DEPTH=10 in walk_dependencies. \
             walk_dependencies bails with CycleDetected before validate_composition \
             even runs; both signal paths converge on the same error variant. \
             Pinned to v3 (see cycle/two-node-symmetric).",
        runner: || -> Outcome {
            // Build a 12-element chain a0 → a1 →... → a11 → a0 (loops back).
            let mut atoms = Vec::new();
            for i in 0..12 {
                let next = if i == 11 { 0 } else { i + 1 };
                atoms.push(synth_atom(
                    &format!("deep_{}", i),
                    AtomRole::Operation,
                    if i == 0 { Some("data:0951") } else { None },
                    if i == 0 { Some("format:3475") } else { None },
                    vec![Box::leak(format!("deep_{}", next).into_boxed_str())],
                ));
            }
            let reg = synth_registry(atoms);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &reg,
                &empty_archetypes(),
                3,
            ) {
                Ok(_) => panic!("deep cycle should fail"),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    // ───────────────── MalformedExclusion (3) ─────────────────
    AdversarialCase {
        id: "malformed-exclusion/excludes-ghost-atom",
        category: Category::MalformedExclusion,
        description: "Single producer of the goal EDAM; its `excludes:` list points at \
             an atom id that doesn't exist in the registry. Validation rule 6 \
             (gate well-formedness) fires — atoms can't `excludes:` a name \
             that won't be loaded.",
        runner: || -> Outcome {
            let mut atom = synth_atom(
                "lonely",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec![],
            );
            atom.excludes = vec!["ghost_that_was_never_authored".into()];
            let atoms = synth_registry(vec![atom]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                2,
            ) {
                Ok(_) => panic!("malformed exclusion should fail"),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    AdversarialCase {
        id: "malformed-exclusion/multiple-ghosts",
        category: Category::MalformedExclusion,
        description: "Atom excludes two non-existent ids; the validator surfaces the first \
             one it encounters — the contract is `MalformedExclusion`, not a \
             cumulative list. Documents that one bad excludes line is enough \
             to block emit.",
        runner: || -> Outcome {
            let mut atom = synth_atom(
                "lonely_two",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec![],
            );
            atom.excludes = vec!["ghost_a".into(), "ghost_b".into()];
            let atoms = synth_registry(vec![atom]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                2,
            ) {
                Ok(_) => panic!("multi-ghost exclusion should fail"),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    AdversarialCase {
        id: "malformed-exclusion/transitive-ghost",
        category: Category::MalformedExclusion,
        description: "Goal-producer's transitive dep declares a ghost exclusion. \
             Even when the ghost lives on a non-leaf atom, validation runs \
             over every composed atom — the dep gets caught too.",
        runner: || -> Outcome {
            let mut leaf = synth_atom("leaf", AtomRole::Operation, None, None, vec![]);
            leaf.excludes = vec!["spectral_ghost".into()];
            let producer = synth_atom(
                "producer",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec!["leaf"],
            );
            let atoms = synth_registry(vec![leaf, producer]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                2,
            ) {
                Ok(_) => panic!("transitive-ghost exclusion should fail"),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    // ───────────────── ExclusionConflict (2) ─────────────────
    AdversarialCase {
        id: "exclusion-conflict/single-producer-pulls-mutually-exclusive-helpers",
        category: Category::ExclusionConflict,
        description: "Only one atom produces the goal, but its two upstream deps `helper_a` \
             and `helper_b` are mutually exclusive (helper_a.excludes = [helper_b]). \
             Backward-chain pulls everything in via walk_dependencies; validation \
             rule 1 catches the conflict at the helper level.",
        runner: || -> Outcome {
            let producer = synth_atom(
                "top_pulls_both",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec!["helper_a", "helper_b"],
            );
            let mut helper_a = synth_atom("helper_a", AtomRole::Operation, None, None, vec![]);
            helper_a.excludes = vec!["helper_b".into()];
            let helper_b = synth_atom("helper_b", AtomRole::Operation, None, None, vec![]);
            let atoms = synth_registry(vec![producer, helper_a, helper_b]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                2,
            ) {
                Ok(r) => panic!(
                    "exclusion-conflict should fail; got Ok with {} atoms",
                    r.atoms.len()
                ),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    AdversarialCase {
        id: "exclusion-conflict/excludes-its-own-dependency",
        category: Category::ExclusionConflict,
        description: "An atom whose `depends_on` and `excludes` list the same id. \
             Walk_dependencies pulls the dep in; validation rule 1 fires \
             on the conflict. Catches the easy authoring mistake of copy-pasting \
             dep + excludes block.",
        runner: || -> Outcome {
            let mut a = synth_atom(
                "self_excl",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec!["dep_helper"],
            );
            a.excludes = vec!["dep_helper".into()];
            let dep = synth_atom("dep_helper", AtomRole::Operation, None, None, vec![]);
            let atoms = synth_registry(vec![a, dep]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                2,
            ) {
                Ok(r) => panic!(
                    "self-conflict should fail; got Ok with {} atoms",
                    r.atoms.len()
                ),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    // ───────────────── NoArchetypeMatch (1 more) ─────────────────
    AdversarialCase {
        id: "no-archetype/empty-atom-registry-empty-archetypes",
        category: Category::NoArchetypeMatch,
        description: "Both registries empty. Backward-chain finds no producer at all → \
             NoArchetypeMatch (the variant doubles as the catch-all when \
             nothing in the registry can plausibly satisfy the goal).",
        runner: || -> Outcome {
            let atoms = synth_registry(vec![]);
            let archetypes = empty_archetypes();
            match compose_with_version(&de_table_goal(), "bioinformatics", &atoms, &archetypes, 2) {
                Ok(_) => panic!("empty registries should yield NoArchetypeMatch"),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    // ───────────────── MethodChoiceUnresolved (2) ─────────────────
    AdversarialCase {
        id: "method-choice/no-discovery-in-chain",
        category: Category::MethodChoiceUnresolved,
        description: "Operation atom defers a method choice to a discovery atom that \
             isn't a transitive dep. Validation rule 5 fires.",
        runner: || -> Outcome {
            use ecaa_workflow_core::atom::MethodChoiceRef;
            let mut a = synth_atom(
                "deferred",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec![],
            );
            a.method_choice = Some(MethodChoiceRef {
                deferred_to: "phantom_discoverer".into(),
            });
            let atoms = synth_registry(vec![a]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                2,
            ) {
                Ok(r) => panic!(
                    "missing-discovery method-choice should fail; got Ok with {} atoms",
                    r.atoms.len()
                ),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    AdversarialCase {
        id: "method-choice/deferred-target-is-operation-not-discovery",
        category: Category::MethodChoiceUnresolved,
        description: "Operation atom defers to another *operation* atom (not a discovery). \
             Even when the target atom IS in the chain, the role check in \
             validation rule 5 fails — only a Discovery atom can resolve a \
             method choice. Catches the role-confusion mistake.",
        runner: || -> Outcome {
            use ecaa_workflow_core::atom::MethodChoiceRef;
            let mut producer = synth_atom(
                "primary",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec!["mis_role"],
            );
            producer.method_choice = Some(MethodChoiceRef {
                deferred_to: "mis_role".into(),
            });
            let target = synth_atom("mis_role", AtomRole::Operation, None, None, vec![]);
            let atoms = synth_registry(vec![producer, target]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                2,
            ) {
                Ok(_) => panic!("operation-as-discovery should fail"),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    // ───────────────── JointSourceMismatch (2) ─────────────────
    AdversarialCase {
        id: "joint-source/diverging-source-atoms",
        category: Category::JointSourceMismatch,
        description: "Atom requires its two upstream inputs to come from the same source \
             via `joint_with`, but the producers' `attributes.source_atom` differ. \
             Validation rule 7 fires — multimodal joint analyses can't silently \
             stitch mismatched runs.",
        runner: || -> Outcome {
            use ecaa_workflow_core::atom::JointlyWithConstraint;
            let mut consumer = synth_atom(
                "joint_consumer",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec!["src_a", "src_b"],
            );
            consumer.joint_with = vec![JointlyWithConstraint {
                lhs: "src_a".into(),
                rhs: "src_b".into(),
            }];
            let mut a = synth_atom("src_a", AtomRole::Operation, None, None, vec![]);
            a.attributes.insert(
                "source_atom".into(),
                serde_json::Value::String("run_one".into()),
            );
            let mut b = synth_atom("src_b", AtomRole::Operation, None, None, vec![]);
            b.attributes.insert(
                "source_atom".into(),
                serde_json::Value::String("run_two".into()),
            );
            let atoms = synth_registry(vec![consumer, a, b]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                2,
            ) {
                Ok(_) => panic!("joint mismatch should fail"),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    AdversarialCase {
        id: "joint-source/missing-source-atom-attribute",
        category: Category::JointSourceMismatch,
        description: "Joint constraint requires both producers to declare a `source_atom` \
             attribute — when one side is absent, the validator treats both as \
             `None`-vs-concrete and fires JointSourceMismatch. Documents that \
             `joint_with` is opt-in on both producers (not just the consumer).",
        runner: || -> Outcome {
            use ecaa_workflow_core::atom::JointlyWithConstraint;
            let mut consumer = synth_atom(
                "joint2",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec!["m_a", "m_b"],
            );
            consumer.joint_with = vec![JointlyWithConstraint {
                lhs: "m_a".into(),
                rhs: "m_b".into(),
            }];
            let mut a = synth_atom("m_a", AtomRole::Operation, None, None, vec![]);
            a.attributes.insert(
                "source_atom".into(),
                serde_json::Value::String("declared_run".into()),
            );
            // m_b lacks source_atom on purpose.
            let b = synth_atom("m_b", AtomRole::Operation, None, None, vec![]);
            let atoms = synth_registry(vec![consumer, a, b]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                2,
            ) {
                Ok(_) => panic!("missing source_atom should fail"),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    // ───────────────── Succeeds — happy path regression ─────────────────
    AdversarialCase {
        id: "succeeds/single-producer-direct-match",
        category: Category::Succeeds,
        description: "Single atom whose edam_data + edam_format exactly match the goal; \
             backward-chain picks it, walk_dependencies sees no deps, validation \
             passes. The minimal happy-path regression for the synth-registry \
             scaffold.",
        runner: || -> Outcome {
            let producer = synth_atom(
                "single_match",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec![],
            );
            let atoms = synth_registry(vec![producer]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                2,
            ) {
                Ok(r) => Outcome::Ok {
                    atom_count: r.atoms.len(),
                },
                Err(e) => panic!("happy path should succeed; got {:?}", e),
            }
        },
    },
    AdversarialCase {
        id: "succeeds/two-step-chain-via-dep",
        category: Category::Succeeds,
        description: "Goal-producer with a single upstream dep that exists in registry. \
             walk_dependencies pulls the dep in; topological order has the dep \
             first then the producer. Both are composed; validation passes.",
        runner: || -> Outcome {
            let upstream = synth_atom("upstream", AtomRole::Operation, None, None, vec![]);
            let producer = synth_atom(
                "two_step_top",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec!["upstream"],
            );
            let atoms = synth_registry(vec![upstream, producer]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                2,
            ) {
                Ok(r) => {
                    assert!(
                        r.atoms.iter().any(|c| c.stage_id.as_str() == "upstream"),
                        "two-step happy path must include upstream dep"
                    );
                    Outcome::Ok {
                        atom_count: r.atoms.len(),
                    }
                }
                Err(e) => panic!("happy path should succeed; got {:?}", e),
            }
        },
    },
    AdversarialCase {
        id: "succeeds/method-choice-resolved-by-discovery",
        category: Category::Succeeds,
        description: "Operation atom with method_choice deferred to a Discovery atom \
             that IS in its dep chain. Validation rule 5 sees the role match \
             and accepts. Documents the contract: method_choice + Discovery \
             role is the runtime-resolution pattern.",
        runner: || -> Outcome {
            use ecaa_workflow_core::atom::MethodChoiceRef;
            let mut op = synth_atom(
                "with_choice",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec!["the_discoverer"],
            );
            op.method_choice = Some(MethodChoiceRef {
                deferred_to: "the_discoverer".into(),
            });
            let discoverer = synth_atom("the_discoverer", AtomRole::Discovery, None, None, vec![]);
            let atoms = synth_registry(vec![op, discoverer]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                2,
            ) {
                Ok(r) => Outcome::Ok {
                    atom_count: r.atoms.len(),
                },
                Err(e) => panic!("happy path should succeed; got {:?}", e),
            }
        },
    },
    // ───────────────── Batch 3 (S7.14 expansion 19 → 34) ─────────────────
    // Adds depth coverage for the categories already seeded. Cases are
    // pinned to v2 (backward-chain + archetype fast-path) or v3 (cycle
    // detection via walk_dependencies) — the same paths batch-1/batch-2
    // exercise. Cycle cases use v3 since the Kahn-algorithm cycle check
    // lives in the backward-chain path; non-cycle error cases use v2.
    AdversarialCase {
        id: "cycle/self-loop-on-goal-producer",
        category: Category::CycleDetected,
        description: "Goal-producer declares itself in its own `depends_on` list. \
             walk_dependencies' visited-set short-circuit means the depth \
             counter never trips, so detect_cycle is the only signal. \
             Documents the self-edge edge case Kahn's-algorithm-on-the- \
             composition-graph catches that walk_dependencies cannot. \
             Pinned to v3 (see cycle/two-node-symmetric).",
        runner: || -> Outcome {
            let producer = synth_atom(
                "self_loop",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec!["self_loop"],
            );
            let atoms = synth_registry(vec![producer]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                3,
            ) {
                Ok(r) => panic!("self-loop should fail; got Ok with {} atoms", r.atoms.len()),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    AdversarialCase {
        id: "cycle/cycle-via-method-choice-target",
        category: Category::CycleDetected,
        description: "Operation A depends on Discovery D; D depends back on A through \
             a deps cycle. Method_choice on A points to D — every wiring is \
             internally legal but the dep-graph cycle still fires \
             before validation rule 5 (method-choice resolution) gets a \
             chance to run. Documents that detect_cycle is the gate, not \
             validate_composition rule 5. Pinned to v3 (see cycle/two-node-symmetric).",
        runner: || -> Outcome {
            use ecaa_workflow_core::atom::MethodChoiceRef;
            let mut op = synth_atom(
                "method_choice_op",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec!["mc_discoverer"],
            );
            op.method_choice = Some(MethodChoiceRef {
                deferred_to: "mc_discoverer".into(),
            });
            let mc = synth_atom(
                "mc_discoverer",
                AtomRole::Discovery,
                None,
                None,
                vec!["method_choice_op"],
            );
            let atoms = synth_registry(vec![op, mc]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                3,
            ) {
                Ok(_) => panic!("method-choice cycle should fail"),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    AdversarialCase {
        id: "exclusion-conflict/diamond-via-two-distinct-paths",
        category: Category::ExclusionConflict,
        description: "Diamond shape: top → left + right; left + right → bottom. \
             `right.excludes = [bottom]` but bottom is pulled in by left. \
             walk_dependencies visits the diamond top-down; both halves \
             land in `composed_ids`; rule 1 sees the conflict.",
        runner: || -> Outcome {
            let bottom = synth_atom("d_bottom", AtomRole::Operation, None, None, vec![]);
            let left = synth_atom("d_left", AtomRole::Operation, None, None, vec!["d_bottom"]);
            let mut right = synth_atom("d_right", AtomRole::Operation, None, None, vec![]);
            right.excludes = vec!["d_bottom".into()];
            let top = synth_atom(
                "d_top",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec!["d_left", "d_right"],
            );
            let atoms = synth_registry(vec![bottom, left, right, top]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                2,
            ) {
                Ok(r) => panic!(
                    "diamond exclusion should fail; got Ok with {} atoms",
                    r.atoms.len()
                ),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    AdversarialCase {
        id: "exclusion-conflict/transitive-deep-chain",
        category: Category::ExclusionConflict,
        description: "5-deep linear chain ending at a goal-producer. The bottom \
             of the chain excludes its grandparent. walk_dependencies \
             pulls the entire chain in; validation rule 1 catches the \
             transitive conflict regardless of the depth.",
        runner: || -> Outcome {
            let leaf = synth_atom("chain_leaf", AtomRole::Operation, None, None, vec![]);
            let l2 = synth_atom(
                "chain_l2",
                AtomRole::Operation,
                None,
                None,
                vec!["chain_leaf"],
            );
            let mut l3 = synth_atom(
                "chain_l3",
                AtomRole::Operation,
                None,
                None,
                vec!["chain_l2"],
            );
            l3.excludes = vec!["chain_leaf".into()];
            let l4 = synth_atom(
                "chain_l4",
                AtomRole::Operation,
                None,
                None,
                vec!["chain_l3"],
            );
            let top = synth_atom(
                "chain_top",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec!["chain_l4"],
            );
            let atoms = synth_registry(vec![leaf, l2, l3, l4, top]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                2,
            ) {
                Ok(_) => panic!("deep transitive-exclusion should fail"),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    AdversarialCase {
        id: "method-choice/typo-in-deferred-target",
        category: Category::MethodChoiceUnresolved,
        description: "method_choice points at a Discovery atom whose id has an \
             extra letter. Validation rule 5 looks for an exact match in \
             the chain (no fuzzy resolution); the typo fires \
             MethodChoiceUnresolved. Catches the most common authoring slip.",
        runner: || -> Outcome {
            use ecaa_workflow_core::atom::MethodChoiceRef;
            let mut op = synth_atom(
                "typo_op",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec!["typo_disc"],
            );
            op.method_choice = Some(MethodChoiceRef {
                deferred_to: "typo_disco".into(), // extra letter
            });
            let disc = synth_atom("typo_disc", AtomRole::Discovery, None, None, vec![]);
            let atoms = synth_registry(vec![op, disc]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                2,
            ) {
                Ok(_) => panic!("typo'd method-choice should fail"),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    AdversarialCase {
        id: "method-choice/discovery-not-in-chain-but-in-registry",
        category: Category::MethodChoiceUnresolved,
        description: "Discovery atom that COULD resolve the choice exists in the \
             registry but isn't in the operation's dep chain — so it's \
             not in `result.atoms`. Validation rule 5 only walks the \
             composed set, not the wider registry. Documents that the \
             method-choice contract requires the resolver to be a \
             transitive dep, not just a registered atom.",
        runner: || -> Outcome {
            use ecaa_workflow_core::atom::MethodChoiceRef;
            let mut op = synth_atom(
                "in_chain_op",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec![],
            );
            op.method_choice = Some(MethodChoiceRef {
                deferred_to: "orphan_disc".into(),
            });
            let orphan = synth_atom("orphan_disc", AtomRole::Discovery, None, None, vec![]);
            let atoms = synth_registry(vec![op, orphan]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                2,
            ) {
                Ok(_) => panic!("orphan discovery should fail"),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    AdversarialCase {
        id: "joint-source/three-way-mismatch-cascades",
        category: Category::JointSourceMismatch,
        description: "Consumer with three joint constraints (a↔b, b↔c, a↔c) where \
             a and b agree but c diverges. The first failing constraint \
             surfaces; the validator stops on the first mismatch. \
             Documents that joint_with isn't a multi-valued aggregator — \
             it's a per-pair equality.",
        runner: || -> Outcome {
            use ecaa_workflow_core::atom::JointlyWithConstraint;
            let mut consumer = synth_atom(
                "tri_consumer",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec!["tri_a", "tri_b", "tri_c"],
            );
            consumer.joint_with = vec![
                JointlyWithConstraint {
                    lhs: "tri_a".into(),
                    rhs: "tri_b".into(),
                },
                JointlyWithConstraint {
                    lhs: "tri_b".into(),
                    rhs: "tri_c".into(),
                },
                JointlyWithConstraint {
                    lhs: "tri_a".into(),
                    rhs: "tri_c".into(),
                },
            ];
            let mut a = synth_atom("tri_a", AtomRole::Operation, None, None, vec![]);
            a.attributes.insert(
                "source_atom".into(),
                serde_json::Value::String("run_X".into()),
            );
            let mut b = synth_atom("tri_b", AtomRole::Operation, None, None, vec![]);
            b.attributes.insert(
                "source_atom".into(),
                serde_json::Value::String("run_X".into()),
            );
            let mut c = synth_atom("tri_c", AtomRole::Operation, None, None, vec![]);
            c.attributes.insert(
                "source_atom".into(),
                serde_json::Value::String("run_Y".into()),
            );
            let atoms = synth_registry(vec![consumer, a, b, c]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                2,
            ) {
                Ok(_) => panic!("3-way joint mismatch should fail"),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    AdversarialCase {
        id: "joint-source/empty-string-source-vs-concrete",
        category: Category::JointSourceMismatch,
        description: "One producer's `source_atom` is an explicit empty string; the \
             other carries a concrete value. The validator compares JSON \
             values directly — empty != \"run_one\" — so JointSourceMismatch \
             fires. Documents that `\"\"` is NOT silently treated as missing.",
        runner: || -> Outcome {
            use ecaa_workflow_core::atom::JointlyWithConstraint;
            let mut consumer = synth_atom(
                "empty_src_consumer",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec!["e_a", "e_b"],
            );
            consumer.joint_with = vec![JointlyWithConstraint {
                lhs: "e_a".into(),
                rhs: "e_b".into(),
            }];
            let mut a = synth_atom("e_a", AtomRole::Operation, None, None, vec![]);
            a.attributes
                .insert("source_atom".into(), serde_json::Value::String("".into()));
            let mut b = synth_atom("e_b", AtomRole::Operation, None, None, vec![]);
            b.attributes.insert(
                "source_atom".into(),
                serde_json::Value::String("run_one".into()),
            );
            let atoms = synth_registry(vec![consumer, a, b]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                2,
            ) {
                Ok(_) => panic!("empty-string source vs concrete should fail"),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    AdversarialCase {
        id: "no-archetype/registry-with-only-discovery-atoms",
        category: Category::NoArchetypeMatch,
        description: "Registry is non-empty but every atom is `role: discovery`. \
             find_producers returns nothing for the goal data + format \
             (discoveries score methods, they don't produce committed \
             artifacts). subtype-producers fallback also misses → \
             NoArchetypeMatch. Documents the contract that backward-chain \
             requires at least one Operation atom in the registry.",
        runner: || -> Outcome {
            let d1 = synth_atom("disc1", AtomRole::Discovery, None, None, vec![]);
            let d2 = synth_atom("disc2", AtomRole::Discovery, None, None, vec![]);
            let atoms = synth_registry(vec![d1, d2]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                2,
            ) {
                Ok(_) => panic!(
                    "discovery-only registry should yield NoArchetypeMatch \
                     for an Operation-typed goal"
                ),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    AdversarialCase {
        id: "goal-unreachable/format-mismatch-via-subtype-fallback",
        category: Category::GoalUnreachable,
        description: "Sole producer matches `goal.edam_data` exactly but its \
             `edam_format` differs. find_producers requires both fields to \
             match (returns empty); subtype-producer fallback uses \
             `is_subtype_of` for the data axis only and pulls the producer \
             in. validate_composition rule 3 then runs the strict (data, \
             format) check and surfaces GoalUnreachable. Documents the \
             rare path where backward-chain composes a result the \
             validator then rejects — the integrated `compose()` reports \
             GoalUnreachable, not NoArchetypeMatch.",
        runner: || -> Outcome {
            let producer = synth_atom(
                "format_mismatch",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:1929"), // not format:3475
                vec![],
            );
            let atoms = synth_registry(vec![producer]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                2,
            ) {
                Ok(r) => panic!(
                    "format-mismatch should fail; got Ok with {} atoms",
                    r.atoms.len()
                ),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    AdversarialCase {
        id: "succeeds/deep-five-step-chain-no-cycle",
        category: Category::Succeeds,
        description: "Deep linear chain of 5 atoms with the producer at the top \
             and a leaf at the bottom. Inverse of the cycle/exceeds-walk- \
             depth case: shallower than MAX_DEPTH=10, so walk_dependencies \
             pulls all five in successfully and validation passes. \
             Documents that depth itself isn't the failure signal — only \
             unbroken cycles trip it.",
        runner: || -> Outcome {
            let l0 = synth_atom("happy_l0", AtomRole::Operation, None, None, vec![]);
            let l1 = synth_atom(
                "happy_l1",
                AtomRole::Operation,
                None,
                None,
                vec!["happy_l0"],
            );
            let l2 = synth_atom(
                "happy_l2",
                AtomRole::Operation,
                None,
                None,
                vec!["happy_l1"],
            );
            let l3 = synth_atom(
                "happy_l3",
                AtomRole::Operation,
                None,
                None,
                vec!["happy_l2"],
            );
            let top = synth_atom(
                "happy_top",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec!["happy_l3"],
            );
            let atoms = synth_registry(vec![l0, l1, l2, l3, top]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                2,
            ) {
                Ok(r) => {
                    assert!(
                        r.atoms.len() >= 5,
                        "deep happy path must compose all 5 atoms; got {}",
                        r.atoms.len()
                    );
                    Outcome::Ok {
                        atom_count: r.atoms.len(),
                    }
                }
                Err(e) => panic!("deep happy path should succeed; got {:?}", e),
            }
        },
    },
    AdversarialCase {
        id: "succeeds/joint-with-matching-sources",
        category: Category::Succeeds,
        description: "Inverse of joint-source/diverging-source-atoms: producers \
             agree on `source_atom` so the validator accepts. \
             Documents the contract for legal multimodal joint analyses \
             — composer permits joint_with when both sides agree.",
        runner: || -> Outcome {
            use ecaa_workflow_core::atom::JointlyWithConstraint;
            let mut consumer = synth_atom(
                "ok_consumer",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec!["ok_a", "ok_b"],
            );
            consumer.joint_with = vec![JointlyWithConstraint {
                lhs: "ok_a".into(),
                rhs: "ok_b".into(),
            }];
            let mut a = synth_atom("ok_a", AtomRole::Operation, None, None, vec![]);
            a.attributes.insert(
                "source_atom".into(),
                serde_json::Value::String("run_alpha".into()),
            );
            let mut b = synth_atom("ok_b", AtomRole::Operation, None, None, vec![]);
            b.attributes.insert(
                "source_atom".into(),
                serde_json::Value::String("run_alpha".into()),
            );
            let atoms = synth_registry(vec![consumer, a, b]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                2,
            ) {
                Ok(r) => Outcome::Ok {
                    atom_count: r.atoms.len(),
                },
                Err(e) => panic!("joint-with-matching should succeed; got {:?}", e),
            }
        },
    },
    AdversarialCase {
        id: "succeeds/wide-fan-out-three-helpers",
        category: Category::Succeeds,
        description: "Producer with three independent dep helpers (no cycles, no \
             excludes, no method_choice). Tests the wide-but-shallow \
             happy path that real archetypes use (e.g. RNA-seq DE with \
             alignment + featureCounts + DESeq2).",
        runner: || -> Outcome {
            let h1 = synth_atom("fan_h1", AtomRole::Operation, None, None, vec![]);
            let h2 = synth_atom("fan_h2", AtomRole::Operation, None, None, vec![]);
            let h3 = synth_atom("fan_h3", AtomRole::Operation, None, None, vec![]);
            let top = synth_atom(
                "fan_top",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec!["fan_h1", "fan_h2", "fan_h3"],
            );
            let atoms = synth_registry(vec![h1, h2, h3, top]);
            match compose_with_version(
                &de_table_goal(),
                "bioinformatics",
                &atoms,
                &empty_archetypes(),
                2,
            ) {
                Ok(r) => {
                    assert!(
                        r.atoms.len() >= 4,
                        "wide-fan-out should pull in all 4 atoms"
                    );
                    Outcome::Ok {
                        atom_count: r.atoms.len(),
                    }
                }
                Err(e) => panic!("wide fan-out happy path should succeed; got {:?}", e),
            }
        },
    },
    // ───────────────── Batch 4 (S7.14 — archetype synth path) ─────────────────
    // These cases route through the archetype fast-path in
    // composer::compose (`archetype_reg.find_match` succeeds) so they
    // exercise the archetype-only failure modes the backward-chain
    // path can't surface: UnknownAtom (scaffold names a missing atom),
    // archetype-level ExclusionConflict (scaffold pulls in atoms that
    // exclude each other), InputUnsatisfied (atom in scaffold has a
    // dep neither in scaffold nor in registry — actually the
    // archetype path treats unknown deps as intake-supplied, so we
    // flip the case to verify the contract: dep IN registry but NOT
    // in scaffold IS UnsatisfiedInputs because `composed_ids` doesn't
    // contain it).
    AdversarialCase {
        id: "unknown-atom/archetype-references-ghost-atom",
        category: Category::UnknownAtom,
        description: "Archetype scaffold lists `ghost_atom_never_authored` but the \
             AtomRegistry has only the goal-producing atom. The archetype \
             path's slot-fill loop hits `atom_reg.get(...)` and the lookup \
             returns None → UnknownAtom { archetype_id, atom_id }. \
             Catches the most common authoring slip: archetype renames \
             an atom but the corresponding atom file isn't yet renamed.",
        runner: || -> Outcome {
            let real = synth_atom(
                "real_producer",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec![],
            );
            let atoms = synth_registry(vec![real]);
            // Archetype scaffolds a ghost id; the schema validates the
            // id pattern (snake_case), but composer's slot-fill catches
            // the missing AtomRegistry entry.
            let arch = synth_archetype(
                "ghost_pointer",
                "data:0951",
                Some("format:3475"),
                "bioinformatics",
                vec!["ghost_atom_never_authored"],
            );
            let archetypes = synth_archetypes(vec![arch]);
            match compose_with_version(&de_table_goal(), "bioinformatics", &atoms, &archetypes, 2) {
                Ok(_) => panic!("ghost-archetype-atom should fail"),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    AdversarialCase {
        id: "unknown-atom/archetype-with-multiple-ghosts",
        category: Category::UnknownAtom,
        description: "Archetype scaffold lists 2 atoms; only one is in the registry. \
             The slot-fill loop iterates over `archetype.atoms` in order, \
             so the FIRST ghost is what fires UnknownAtom. Catches the \
             behavior that the validator surfaces the first failure, not \
             a cumulative list — useful contract for diagnostic clarity.",
        runner: || -> Outcome {
            let real = synth_atom(
                "real_one",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec![],
            );
            let atoms = synth_registry(vec![real]);
            let arch = synth_archetype(
                "two_ghosts",
                "data:0951",
                Some("format:3475"),
                "bioinformatics",
                vec!["real_one", "first_ghost", "second_ghost"],
            );
            let archetypes = synth_archetypes(vec![arch]);
            match compose_with_version(&de_table_goal(), "bioinformatics", &atoms, &archetypes, 2) {
                Ok(_) => panic!("multi-ghost archetype should fail"),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    AdversarialCase {
        id: "input-unsatisfied/archetype-omits-required-dep",
        category: Category::InputUnsatisfied,
        description: "Archetype lists atom A whose `depends_on = [helper]`, and the \
             `helper` atom IS in the registry but the archetype scaffold \
             does NOT include it. validate_composition rule 4 catches it: \
             `composed_ids` lacks `helper` even though `atom_reg.get(\
             \"helper\")` is Some. Documents the contract: every dep an \
             archetype's atom names must be either in the scaffold or \
             treated as intake-supplied (absent from registry).",
        runner: || -> Outcome {
            let helper = synth_atom("helper_dep", AtomRole::Operation, None, None, vec![]);
            let mut producer = synth_atom(
                "needs_helper",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec![],
            );
            // Inject the dep AFTER synth_atom so the archetype scaffold
            // can be authored separately.
            producer.depends_on = vec!["helper_dep".into()];
            let atoms = synth_registry(vec![helper, producer]);
            let arch = synth_archetype(
                "omits_helper",
                "data:0951",
                Some("format:3475"),
                "bioinformatics",
                vec!["needs_helper"], // 'helper_dep' missing from scaffold
            );
            let archetypes = synth_archetypes(vec![arch]);
            match compose_with_version(&de_table_goal(), "bioinformatics", &atoms, &archetypes, 2) {
                Ok(_) => panic!("archetype omitting required dep should fail"),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    AdversarialCase {
        id: "exclusion-conflict/archetype-pulls-in-mutually-exclusive-atoms",
        category: Category::ExclusionConflict,
        description: "Archetype scaffold pulls in two atoms that mutually exclude \
             each other. The archetype path loads both into `composed`; \
             validation rule 1 fires with `archetype_id = <real id>` not \
             `<backward-chain>` — verifies the contract that the error \
             carries the matched archetype's id when the archetype path \
             produced the conflict.",
        runner: || -> Outcome {
            let mut a = synth_atom(
                "excl_left",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec![],
            );
            a.excludes = vec!["excl_right".into()];
            let b = synth_atom("excl_right", AtomRole::Operation, None, None, vec![]);
            let atoms = synth_registry(vec![a, b]);
            let arch = synth_archetype(
                "exclusion_pair",
                "data:0951",
                Some("format:3475"),
                "bioinformatics",
                vec!["excl_left", "excl_right"],
            );
            let archetypes = synth_archetypes(vec![arch]);
            match compose_with_version(&de_table_goal(), "bioinformatics", &atoms, &archetypes, 2) {
                Ok(_) => panic!("archetype with mutually-excluding atoms should fail"),
                Err(e) => Outcome::Err(e),
            }
        },
    },
    AdversarialCase {
        id: "succeeds/archetype-fast-path-matches-and-composes",
        category: Category::Succeeds,
        description: "Two atoms (helper + producer), an archetype that names both. \
             The archetype path matches on (data, format, project_class), \
             produces a CompositionResult with `matched_archetype = Some(\
             id)`. Documents the happy archetype path: clean compose with \
             a non-None matched_archetype field.",
        runner: || -> Outcome {
            let helper = synth_atom("arch_helper", AtomRole::Operation, None, None, vec![]);
            let mut producer = synth_atom(
                "arch_top",
                AtomRole::Operation,
                Some("data:0951"),
                Some("format:3475"),
                vec![],
            );
            producer.depends_on = vec!["arch_helper".into()];
            let atoms = synth_registry(vec![helper, producer]);
            let arch = synth_archetype(
                "happy_arch",
                "data:0951",
                Some("format:3475"),
                "bioinformatics",
                vec!["arch_helper", "arch_top"],
            );
            let archetypes = synth_archetypes(vec![arch]);
            match composer::compose(&de_table_goal(), "bioinformatics", &atoms, &archetypes) {
                Ok(r) => {
                    assert!(
                        r.matched_archetype.is_some(),
                        "archetype-fast-path must populate matched_archetype"
                    );
                    Outcome::Ok {
                        atom_count: r.atoms.len(),
                    }
                }
                Err(e) => panic!("archetype happy path should succeed; got {:?}", e),
            }
        },
    },
];

#[test]
fn every_case_runs_to_expected_outcome() {
    for case in CASES {
        let outcome = (case.runner)();
        match (case.category, &outcome) {
            (Category::Succeeds, Outcome::Ok { atom_count }) => {
                assert!(
                    *atom_count > 0,
                    "{}: expected at least one composed atom on a succeeds case",
                    case.id
                );
            }
            (
                Category::NoArchetypeMatch,
                Outcome::Err(CompositionError::NoArchetypeMatch { .. }),
            ) => {}
            (
                Category::TieRequiresSmeDecision,
                Outcome::Err(CompositionError::TieRequiresSmeDecision { .. }),
            ) => {}
            (Category::TieRequiresSmeDecision, Outcome::Ok { .. }) => {
                // Today's catalogue has only one matching archetype
                // for the seed goal so the tie path is dormant; the
                // case documents the seed. When a second archetype
                // ships, this branch becomes unreachable and the
                // runner returns Err.
            }
            (
                Category::ExclusionConflict,
                Outcome::Err(CompositionError::ExclusionConflict { .. }),
            ) => {}
            (Category::CycleDetected, Outcome::Err(CompositionError::CycleDetected { .. })) => {}
            (Category::GoalUnreachable, Outcome::Err(CompositionError::GoalUnreachable { .. })) => {
            }
            (
                Category::InputUnsatisfied,
                Outcome::Err(CompositionError::InputUnsatisfied { .. }),
            ) => {}
            (
                Category::MethodChoiceUnresolved,
                Outcome::Err(CompositionError::MethodChoiceUnresolved { .. }),
            ) => {}
            (
                Category::MalformedExclusion,
                Outcome::Err(CompositionError::MalformedExclusion { .. }),
            ) => {}
            (Category::UnknownAtom, Outcome::Err(CompositionError::UnknownAtom { .. })) => {}
            (
                Category::UnfilledRequiredSlot,
                Outcome::Err(CompositionError::UnfilledRequiredSlot { .. }),
            ) => {}
            (
                Category::JointSourceMismatch,
                Outcome::Err(CompositionError::JointSourceMismatch { .. }),
            ) => {}
            (cat, outcome) => panic!(
                "{}: case category {:?} did not match outcome {}",
                case.id,
                cat,
                match outcome {
                    Outcome::Ok { atom_count } => format!("Ok({} atoms)", atom_count),
                    Outcome::Err(e) => format!("Err({:?})", e),
                }
            ),
        }
    }
}

#[test]
fn seed_batch_provides_at_least_one_case_per_seeded_category() {
    // Tracks the coverage tracker in the file header. When a category
    // moves from 0 → 1+ seeded cases, add it to this list. The list
    // documents *what we have* — categories with 0 seeded cases stay
    // out of this list intentionally.
    let seeded_today: &[Category] = &[
        Category::NoArchetypeMatch,
        Category::TieRequiresSmeDecision,
        Category::CycleDetected,
        Category::MalformedExclusion,
        Category::ExclusionConflict,
        Category::MethodChoiceUnresolved,
        Category::JointSourceMismatch,
        Category::GoalUnreachable,
        Category::UnknownAtom,
        Category::InputUnsatisfied,
        Category::Succeeds,
    ];
    for cat in seeded_today {
        let found = CASES.iter().any(|c| c.category == *cat);
        assert!(
            found,
            "no seed case carries category {:?}; coverage tracker drift",
            cat
        );
    }
}

#[test]
fn case_ids_are_unique() {
    use std::collections::BTreeSet;
    let mut ids = BTreeSet::new();
    for case in CASES {
        assert!(
            ids.insert(case.id),
            "duplicate adversarial-case id: {}",
            case.id
        );
    }
}

