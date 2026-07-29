//! F15 — atom-count drift gate.
//!
//! `config/stage-atoms/*.yaml` is the composer's atom catalog. CLAUDE.md
//! used to hard-code an integer literal ("39 typed atom files") that
//! rotted between releases (was 39, then 45). This test couples the
//! atom-file count to a single in-repo source of truth — the
//! `EXPECTED_STAGE_ATOMS` constant below — so:
//!
//! - Adding an atom YAML without bumping the constant ⇒ this test fails.
//! - Bumping the constant without adding an atom YAML ⇒ this test fails.
//!
//! The fix is to update both in the same change. This test is intentionally
//! SELF-CONTAINED: it depends only on files inside this repository (the
//! `config/stage-atoms/` directory and this constant), so it runs in every
//! checkout — including the OSS repo — and actually guards atom-catalogue-size
//! drift. It used to depend on `.github/ci/expected-test-counts.json`, which is
//! absent from the OSS repo, so it was `#[ignore]`d and never ran.
//! CLAUDE.md no longer carries the integer literal.
//!
//! GUARDS ATOM-CATALOGUE-SIZE DRIFT: `EXPECTED_STAGE_ATOMS` must be bumped
//! INTENTIONALLY in the same change that adds or removes an atom YAML under
//! `config/stage-atoms/`. Do not "fix" a failure by blindly editing this
//! number — a mismatch means the catalogue changed and that change must be
//! deliberate.

use std::fs;
use std::path::Path;

/// Expected number of atom YAMLs under `config/stage-atoms/` (excluding
/// `_`-prefixed partials). Bump this only when atoms are intentionally
/// added or removed.
const EXPECTED_STAGE_ATOMS: usize = 105;

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn count_stage_atoms() -> usize {
    let dir = repo_root().join("config/stage-atoms");
    fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.ends_with(".yaml") && !name.starts_with('_')
        })
        .count()
}

#[test]
fn atom_count_matches_baseline() {
    let actual = count_stage_atoms();
    assert_eq!(
        actual, EXPECTED_STAGE_ATOMS,
        "config/stage-atoms/*.yaml count {actual} differs from expected \
         {EXPECTED_STAGE_ATOMS}. This gate guards atom-catalogue-size drift: \
         if you intentionally added or removed an atom YAML, bump \
         `EXPECTED_STAGE_ATOMS` in this test in the same change — F15."
    );
}

/// The atom schema is `additionalProperties:false`, so any new top-level
/// field an atom carries must be enumerated in `_atom.schema.json`. This
/// gate couples the `interpretation_exempt_from_word_budget` field used by
/// `biological_interpretation` + `final_reporting` to its schema property.
#[test]
fn atom_schema_declares_interpretation_word_budget_flag() {
    let schema_path = repo_root().join("config/stage-atoms/_atom.schema.json");
    let raw = fs::read_to_string(&schema_path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", schema_path.display()));
    let schema: serde_json::Value = serde_json::from_str(&raw).expect("schema is valid JSON");
    let prop = &schema["properties"]["interpretation_exempt_from_word_budget"];
    assert_eq!(
        prop["type"], "boolean",
        "schema must declare interpretation_exempt_from_word_budget as a boolean property; \
         the atom schema is additionalProperties:false so the field must be enumerated"
    );
}

/// The method-neutral `biological_interpretation` atom: operation role,
/// agent assignee, word-budget exempt, figure-exempt, an OPTIONAL
/// literature_concordance input port, and a claim_boundary that requires
/// result-row/PMID anchoring while forbidding method recommendation.
#[test]
fn biological_interpretation_atom_is_well_formed() {
    let path = repo_root().join("config/stage-atoms/biological_interpretation.yaml");
    let raw =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let atom: serde_yaml_ng::Value = serde_yaml_ng::from_str(&raw).expect("atom YAML parses");
    assert_eq!(atom["id"], "biological_interpretation");
    assert_eq!(atom["role"], "operation");
    assert_eq!(atom["assignee"], "agent");
    assert_eq!(atom["interpretation_exempt_from_word_budget"], true);
    // figure_exempt block is present (interpretation produces structured text only).
    assert!(
        atom["figure_exempt"]["reason"].is_string(),
        "biological_interpretation must be figure_exempt"
    );
    // The optional literature port mirrors the upstream atom's output type.
    let inputs = atom["inputs"].as_sequence().expect("inputs array");
    let lit = inputs
        .iter()
        .find(|p| p["name"] == "literature_concordance")
        .expect("optional literature_concordance input port present");
    assert_eq!(lit["semantic_type"]["iri"], "ecaax:claims_evidence_matrix");
    assert_eq!(lit["cardinality"]["kind"], "optional");
    // result.json + interpretation.md are the headline outputs.
    let artifacts = atom["expected_artifacts"]
        .as_sequence()
        .expect("expected_artifacts");
    let names: Vec<&str> = artifacts.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        names.contains(&"interpretation.md"),
        "must emit interpretation.md"
    );
    assert!(names.contains(&"result.json"), "must emit result.json");
    // claim_boundary forbids method recommendation + requires evidence anchors.
    let boundary = atom["claim_boundary"]
        .as_str()
        .expect("claim_boundary string");
    assert!(
        boundary.contains("result row") || boundary.contains("result-table row"),
        "claim_boundary must require every claim cite a result row"
    );
    assert!(
        boundary.contains("PMID"),
        "claim_boundary must mention PMID anchoring"
    );
    assert!(
        boundary.to_lowercase().contains("recommend"),
        "claim_boundary must forbid method recommendation"
    );
}

/// `final_reporting` is reframed findings-first and exempt from the
/// agent prompt's narrative word cap (its prose length scales with the
/// number of result rows it grounds).
#[test]
fn final_reporting_is_findings_first_and_word_budget_exempt() {
    let path = repo_root().join("config/stage-atoms/final_reporting.yaml");
    let raw =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let atom: serde_yaml_ng::Value = serde_yaml_ng::from_str(&raw).expect("final_reporting parses");
    assert_eq!(
        atom["interpretation_exempt_from_word_budget"], true,
        "final_reporting must be exempt from the narrative word cap"
    );
    let boundary = atom["claim_boundary"]
        .as_str()
        .expect("claim_boundary string");
    assert!(
        boundary.to_lowercase().contains("findings first")
            || boundary.to_lowercase().contains("findings-first"),
        "final_reporting claim_boundary must be findings-first reframed"
    );
    assert!(
        boundary.contains("result row") || boundary.contains("interpretation"),
        "findings-first reframe must reference result rows / interpretation"
    );
}

/// WS-2 — the task-execution prompt must carve out interpretation /
/// report stages from the ~500-word narrative cap, or the agent
/// truncates result-grounded citations.
#[test]
fn task_execution_prompt_exempts_interpretation_from_word_cap() {
    let path = repo_root().join("scripts/agent-prompts/task-execution.md");
    let text =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    assert!(
        text.contains("interpretation_exempt_from_word_budget"),
        "prompt must name the exemption field"
    );
    assert!(
        text.to_lowercase().contains("interpretation") && text.contains("~500 words"),
        "prompt must scope the ~500-word cap so interpretation stages are exempt"
    );
}

/// WS-2 — biological_interpretation + validate_biological_interpretation
/// must be OPTIONAL (never forbidden) in every interpretation-relevant
/// corpus scenario, so a flag-on emission that includes them still
/// matches the expected-DAG spec.
#[test]
fn interpretation_atoms_optional_in_relevant_scenarios() {
    const INTERP_SCENARIOS: &[&str] = &[
        "atac-buenrostro-immune-lineage",
        "atac-corces-pancancer",
        "bulk-rnaseq-recount3-airway",
        "bulk-rnaseq-recount3-parathyroid",
        "chip-seq-encode-ctcf-k562",
        "chip-seq-mikkelsen-mouse-es",
        "variant-germline-1000g-trio",
        "variant-germline-exome-mendelian",
    ];

    let path = repo_root().join("testdata/dag-correctness-corpus/MANIFEST.yaml");
    let raw =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&raw).expect("manifest parses");
    let scenarios = doc["scenarios"].as_sequence().expect("scenarios array");

    for sid in INTERP_SCENARIOS {
        let scenario = scenarios
            .iter()
            .find(|s| s["id"] == *sid)
            .unwrap_or_else(|| panic!("scenario {sid} not found"));
        let opt: Vec<&str> = scenario["expected_dag"]["optional_atoms"]
            .as_sequence()
            .map(|s| s.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let forb: Vec<&str> = scenario["expected_dag"]["forbidden_atoms"]
            .as_sequence()
            .map(|s| s.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        for atom in [
            "biological_interpretation",
            "validate_biological_interpretation",
        ] {
            assert!(
                opt.contains(&atom),
                "{sid}: {atom} must be in optional_atoms"
            );
            assert!(!forb.contains(&atom), "{sid}: {atom} must NOT be forbidden");
        }
    }
}
