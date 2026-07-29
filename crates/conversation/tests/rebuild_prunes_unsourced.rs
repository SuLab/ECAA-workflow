//! A5 integration test — `rebuild_dag` deterministically prunes atoms
//! whose REQUIRED input ports cannot be sourced, INDEPENDENT of the SME
//! exclusion list.
//!
//! Scenario (Pasilla-like): a bulk RNA-seq differential-expression intent
//! with NO gene-set collection registered. The `bulk_rnaseq_de` archetype
//! includes `pathway_enrichment`, which (after task A1) declares a
//! REQUIRED `gene_set_collection` input typed `data:2600`. With no
//! gene-set registered, the composer's `source_typing` pass surfaces no
//! `data:2600` output on the `data_acquisition` anchor, so the input is
//! unsourceable. `rebuild_dag`'s new `prune_unsourced_atoms_pass` (wired
//! after `prune_excluded_atoms`) must drop `pathway_enrichment` even
//! though `excluded_atoms` is empty.
//!
//! Authored under the deferred-build policy: this test is NOT run here.

use ecaa_workflow_conversation::session::Session;
use ecaa_workflow_conversation::tools::{dispatch_one, BatchableTool, Tool, ToolContext};
use std::path::{Path, PathBuf};

/// Repo `config/` dir (archetypes + stage-atoms) the composer loads from.
fn config_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

fn ctx() -> ToolContext {
    ToolContext::new(config_dir(), "claude-sonnet-5")
}

/// Collect every node id present in the authoritative `workflow_dag`.
fn workflow_dag_node_ids(s: &Session) -> Vec<String> {
    s.workflow_dag
        .as_ref()
        .map(|wf| wf.nodes.iter().map(|n| n.id.clone()).collect())
        .unwrap_or_default()
}

// NOTE: `pathway_enrichment`'s `gene_set_collection` input is now OPTIONAL, so
// the unsourced-prune pass no longer drops enrichment when no gene-set is
// registered (gene-set was the sole prunable input class). Enrichment instead
// survives in its own network-egress stage that offline replay SKIPS. This test
// asserts that SURVIVAL (the former "prunes when unsourced" behavior is
// intentionally reversed to keep the Enrichr call out of the shared `reporting`
// atom).
#[tokio::test]
async fn rebuild_dag_keeps_egress_pathway_enrichment_when_no_gene_set_registered() {
    let mut s = Session::new(false);

    // No gene-set collection is registered (no intake `available_data`
    // entry / no `set_intake_field`). The exclusion list stays empty.
    assert!(
        s.excluded_atoms.is_empty(),
        "precondition: excluded_atoms must be empty so the prune cannot be \
         attributed to an SME exclusion"
    );

    // Drive the public intake path; `append_intake_prose` composes the
    // DAG and calls the internal `rebuild_dag` (which now runs the
    // unsourced-prune pass after the exclusion prune).
    let res = dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "bulk rna-seq differential expression between two conditions \
                    in Drosophila samples"
                .into(),
        }),
        &mut s,
        &ctx(),
    )
    .await;
    assert!(
        !res.is_error,
        "intake prose must classify + build a DAG: {res:?}"
    );

    // The exclusion list must STILL be empty — proving the prune below
    // is driven by the unsourced-input check, not by an SME exclusion.
    assert!(
        s.excluded_atoms.is_empty(),
        "excluded_atoms must remain empty across the rebuild"
    );

    // The authoritative workflow_dag MUST contain `pathway_enrichment`. Its
    // `gene_set_collection` input is now OPTIONAL (the default Enrichr tool
    // fetches gene sets over the network egress the atom declares), so the
    // unsourced-prune pass no longer drops enrichment when no local gene-set is
    // registered. Instead the stage survives in its own egress-declared stage
    // and offline replay SKIPS it as not-offline-reproducible — rather than the
    // Enrichr call collapsing into the shared `reporting` atom (which would
    // run-and-fail offline replay).
    let wf_ids = workflow_dag_node_ids(&s);
    assert!(
        !wf_ids.is_empty(),
        "a workflow_dag must have been composed for the bulk RNA-seq intent"
    );
    assert!(
        wf_ids.iter().any(|id| id == "pathway_enrichment"),
        "pathway_enrichment must SURVIVE (gene_set_collection is optional; Enrichr \
         supplies gene sets over network egress); workflow_dag nodes = {wf_ids:?}"
    );

    // The lowered cache (`session.dag`) must agree — the pathway_enrichment
    // task survives the re-lowering.
    if let Some(dag) = s.dag.as_ref() {
        assert!(
            dag.tasks.keys().any(|k| k.as_str() == "pathway_enrichment"),
            "lowered dag must retain a pathway_enrichment task; tasks = {:?}",
            dag.tasks.keys().collect::<Vec<_>>()
        );
    }

    // Sanity: the upstream DE atom that pathway_enrichment depended on
    // must survive — the prune is targeted, not a wholesale teardown.
    assert!(
        wf_ids.iter().any(|id| id == "differential_expression"),
        "differential_expression must survive the unsourced prune; \
         workflow_dag nodes = {wf_ids:?}"
    );
}
