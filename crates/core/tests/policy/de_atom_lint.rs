//! Catalog lint: an atom that lists a count-based GLM tool (DESeq2 /
//! edgeR / limma-voom) in `candidate_tools` fits its dispersion model on
//! RAW integer counts — silently feeding it a normalized/log matrix
//! instead of raw counts produces garbage without erroring. Rank-based
//! tools (Wilcoxon/MAST) carry no such requirement.
//!
//! `validate_de_substrate` catches the raw-counts gap at registry load:
//! any atom that offers one of these tools in `attributes.candidate_tools`
//! must declare an input port whose `statistical_state` is `raw_counts`.
//! Generalized as a small `method -> required substrate` table so a
//! future tool with a different substrate requirement is a one-line
//! table entry, not a bespoke check.

use ecaa_workflow_core::atom::AtomDefinition;
use ecaa_workflow_core::atom_registry::{validate_de_substrate, AtomRegistry};
use ecaa_workflow_core::workflow_contracts::port::{Cardinality, PortContract};
use std::path::{Path, PathBuf};

fn config_stage_atoms() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/stage-atoms")
}

fn raw_counts_port() -> PortContract {
    PortContract {
        name: "raw_counts".into(),
        statistical_state: Some("raw_counts".into()),
        cardinality: Cardinality::Optional,
        ..PortContract::default()
    }
}

fn normalized_counts_port() -> PortContract {
    PortContract {
        name: "normalized_counts".into(),
        statistical_state: Some("normalized".into()),
        cardinality: Cardinality::Optional,
        ..PortContract::default()
    }
}

fn atom_with_tools(id: &str, tools: &[&str]) -> AtomDefinition {
    let mut atom = AtomDefinition::test_default(id);
    atom.attributes
        .insert("candidate_tools".into(), serde_json::json!(tools));
    atom
}

#[test]
fn de_atom_with_count_glm_requires_raw_input_port() {
    let mut de = atom_with_tools("de_probe_no_raw", &["deseq2", "edger", "limma_voom"]);
    de.inputs = vec![normalized_counts_port()];
    de.input_groups.clear();

    let err = validate_de_substrate(&de)
        .expect_err("count-GLM candidate_tools without a raw_counts port must be rejected");
    assert!(
        err.contains("raw_counts"),
        "lint must name the missing raw substrate: {err}"
    );
}

#[test]
fn de_atom_with_count_glm_and_raw_port_passes() {
    let mut de = atom_with_tools("de_probe_with_raw", &["deseq2"]);
    de.inputs = vec![raw_counts_port(), normalized_counts_port()];

    validate_de_substrate(&de).expect("raw_counts port present, lint must pass");
}

#[test]
fn atom_without_count_glm_tools_is_exempt() {
    // wilcoxon / mast carry no raw-counts requirement — the lint only
    // fires for the tools listed in METHOD_REQUIRED_SUBSTRATE.
    let mut de = atom_with_tools("de_probe_rank_only", &["wilcoxon", "mast"]);
    de.inputs = vec![normalized_counts_port()];

    validate_de_substrate(&de).expect("non-count-GLM candidate tools must not require raw_counts");
}

#[test]
fn atom_with_no_candidate_tools_is_exempt() {
    let de = AtomDefinition::test_default("de_probe_no_tools");
    validate_de_substrate(&de).expect("an atom with no candidate_tools attribute must lint-pass");
}

#[test]
fn registry_load_refuses_a_count_glm_atom_missing_raw_counts() {
    let mut atom = atom_with_tools("de_probe_registry_reject", &["deseq2"]);
    atom.inputs = vec![normalized_counts_port()];

    let registry = AtomRegistry::default().with_promoted_overlay([atom]);
    let err = registry
        .validate_consistency()
        .expect_err("registry must refuse to load a count-GLM atom without a raw_counts port");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("raw_counts"),
        "registry-load error must name the missing substrate: {msg}"
    );
}

#[test]
fn real_de_atom_passes_lint() {
    let reg = AtomRegistry::load_from_dir(&config_stage_atoms()).expect("real catalog must load");
    // load_from_dir succeeding + validate_consistency succeeding proves the
    // lint passed for the real catalog's differential_expression atom.
    reg.validate_consistency()
        .expect("real catalog must pass the DE-substrate lint");
    let de = reg
        .get("differential_expression")
        .expect("differential_expression atom must be in the catalog");
    validate_de_substrate(de).expect("the shipped DE atom must pass the lint directly too");
}

/// RCA I-10: `stated_outcome` is agent-recorded ONLY for a task-named
/// regression outcome (omitted for a plain DE-by-condition contrast). The
/// atom's `result_contract.record_in_result_json` checklist must NOT list
/// it unconditionally — an agent following that checklist literally would
/// always populate the field, arming the skip-gated
/// `response_matches_stated_outcome` domain-correctness check for the
/// common case that must stay skipped. It must instead appear under a
/// separate `record_when_applicable` map documenting the precondition.
#[test]
fn stated_outcome_is_conditional_not_on_the_unconditional_checklist() {
    let reg = AtomRegistry::load_from_dir(&config_stage_atoms()).expect("real catalog must load");
    let de = reg
        .get("differential_expression")
        .expect("differential_expression atom must be in the catalog");
    let result_contract = de
        .attributes
        .get("result_contract")
        .expect("differential_expression must declare attributes.result_contract");

    let unconditional = result_contract
        .get("record_in_result_json")
        .and_then(|v| v.as_array())
        .expect("result_contract.record_in_result_json must be an array");
    assert!(
        !unconditional
            .iter()
            .any(|v| v.as_str() == Some("stated_outcome")),
        "stated_outcome must NOT be in the unconditional record_in_result_json checklist \
         (RCA I-10): {unconditional:?}"
    );

    let conditional = result_contract
        .get("record_when_applicable")
        .expect("result_contract.record_when_applicable must document conditional fields");
    assert!(
        conditional.get("stated_outcome").is_some(),
        "stated_outcome must be documented under record_when_applicable: {conditional:?}"
    );
}
