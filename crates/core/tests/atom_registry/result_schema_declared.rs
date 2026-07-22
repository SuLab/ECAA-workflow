//! Task 2 — the two tabular terminal analytical atoms
//! (`differential_expression`, `pathway_enrichment`) declare a
//! `result_schema` block naming their real output columns, so the
//! report-data assembler (Phase 2) can read them by name instead of by
//! position. `variant_calling` emits a VCF, not a TSV, and intentionally
//! declares no `result_schema` — out of scope for the tabular assembler.

use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::report_contract::Comparator;
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
fn terminal_atoms_declare_result_schema() {
    let reg = AtomRegistry::load_from_dir(&config_stage_atoms()).expect("registry loads");

    let de = reg
        .get("differential_expression")
        .expect("differential_expression atom present")
        .result_schema
        .as_ref()
        .expect("differential_expression declares result_schema");
    assert_eq!(de.artifact, "de_results.tsv");
    assert_eq!(de.entity_column, "gene");
    assert_eq!(de.signed_effect_column.as_deref(), Some("log2FoldChange"));
    let de_sig = de
        .significance
        .as_ref()
        .expect("differential_expression declares significance");
    assert_eq!(de_sig.column, "padj");
    assert_eq!(de_sig.threshold, 0.05);
    assert_eq!(de_sig.comparator, Comparator::Lt);

    let pw = reg
        .get("pathway_enrichment")
        .expect("pathway_enrichment atom present")
        .result_schema
        .as_ref()
        .expect("pathway_enrichment declares result_schema");
    assert_eq!(pw.artifact, "pathway_results.tsv");
    assert_eq!(pw.entity_column, "pathway");
    assert_eq!(pw.signed_effect_column.as_deref(), Some("NES"));
    assert_eq!(pw.grouping_column.as_deref(), Some("collection"));
    let pw_sig = pw
        .significance
        .as_ref()
        .expect("pathway_enrichment declares significance");
    assert_eq!(pw_sig.column, "padj");
    assert_eq!(pw_sig.threshold, 0.25);
    assert_eq!(pw_sig.comparator, Comparator::Lt);

    // variant_calling emits a VCF, not a TSV — must NOT declare a result_schema.
    let vc = reg
        .get("variant_calling")
        .expect("variant_calling atom present");
    assert!(
        vc.result_schema.is_none(),
        "variant_calling emits a VCF, not tabular text — it must not declare result_schema"
    );
}
