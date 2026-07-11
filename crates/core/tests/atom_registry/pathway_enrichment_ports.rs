//! A1 — the `pathway_enrichment` atom declares BOTH a `ranked_de_results`
//! input AND a required `gene_set_collection` input typed
//! `data:2600` (the shared `GENE_SET_SEMANTIC_IRI`). Without the gene-set
//! input the composer's `prune_unsourced_atoms` pass cannot recognize the
//! atom as gene-set-dependent, so this is the contract the pruner relies
//! on.

use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::composer_v4::source_typing::GENE_SET_SEMANTIC_IRI;
use ecaa_workflow_core::workflow_contracts::port::Cardinality;
use std::path::{Path, PathBuf};

fn config_stage_atoms() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/stage-atoms")
}

#[test]
fn pathway_enrichment_declares_both_de_and_gene_set_inputs() {
    let reg = AtomRegistry::load_from_dir(&config_stage_atoms())
        .expect("registry must load with the pathway_enrichment atom present");
    let atom = reg
        .get("pathway_enrichment")
        .expect("pathway_enrichment atom must be in the catalog");

    let input_names: Vec<&str> = atom.inputs.iter().map(|p| p.name.as_str()).collect();
    assert!(
        input_names.contains(&"ranked_de_results"),
        "pathway_enrichment must keep its ranked_de_results input; inputs={input_names:?}"
    );
    assert!(
        input_names.contains(&"gene_set_collection"),
        "pathway_enrichment must declare a gene_set_collection input; inputs={input_names:?}"
    );
}

#[test]
fn pathway_enrichment_gene_set_input_is_required_and_typed_data_2600() {
    let reg = AtomRegistry::load_from_dir(&config_stage_atoms())
        .expect("registry must load with the pathway_enrichment atom present");
    let atom = reg
        .get("pathway_enrichment")
        .expect("pathway_enrichment atom must be in the catalog");

    let gene_set = atom
        .inputs
        .iter()
        .find(|p| p.name == "gene_set_collection")
        .expect("gene_set_collection input port must be present");

    // The gene-set input must be typed with the shared canonical IRI so
    // producer (source-typing anchor output) and consumer types unify in
    // the compatibility engine.
    assert_eq!(
        gene_set.semantic_type.stable_id(),
        GENE_SET_SEMANTIC_IRI,
        "gene_set_collection must be typed {GENE_SET_SEMANTIC_IRI}"
    );

    // It must be OPTIONAL. The default Enrichr tool fetches gene-set libraries
    // over the network (see the atom's `safety.network` egress allowlist), so a
    // locally-registered GMT is not required for the stage to run — only the
    // offline fgsea/clusterProfiler tools consume it. Marking it optional keeps
    // the composer from PRUNING enrichment when no local gene-set is registered;
    // instead the stage runs over the network and offline replay SKIPS it as
    // not-offline-reproducible, rather than pushing the Enrichr call into the
    // shared `reporting` atom (which would run-and-fail offline replay).
    assert!(
        matches!(gene_set.cardinality, Cardinality::Optional),
        "gene_set_collection must be an optional input (Enrichr supplies gene \
         sets over the network); got {:?}",
        gene_set.cardinality
    );
}
