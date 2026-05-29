//! Strong cross-omics markers like "multiome", "share-seq", "joint
//! embedding" must NOT lift the `(single_cell_rnaseq, bulk_rnaseq)`
//! suppression. Multiome ARC and SHARE-seq are 2-way protocols
//! (sc_rna + atac) — bulk_rnaseq as a third companion is a false
//! positive that set-equality-mismatches the right cross-omics
//! archetype.

use ecaa_workflow_core::classify::Classifier;
use std::path::Path;

fn classifier() -> Classifier {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
        .join("modality-keywords.yaml");
    Classifier::load(&path).expect("load classifier")
}

#[test]
fn multiome_arc_does_not_surface_bulk_rnaseq_companion() {
    let prompt = "10x Genomics Multiome ARC data — paired single-nucleus \
                  RNA and single-nucleus ATAC from the same nuclei on \
                  human PBMCs. We want a single joint embedding across \
                  the two modalities (WNN-style), cluster annotation, \
                  per-cluster differential expression.";
    let r = classifier().classify(prompt);
    assert_eq!(r.modality, "single_cell_rnaseq");
    let companion_ids: Vec<&str> = r
        .additional_modalities
        .iter()
        .map(|m| m.modality.as_str())
        .collect();
    assert!(
        companion_ids.contains(&"atac_seq"),
        "atac_seq must be a companion, got {:?}",
        companion_ids
    );
    assert!(
        !companion_ids.contains(&"bulk_rnaseq"),
        "bulk_rnaseq must NOT be a companion on multiome prompt; got {:?}",
        companion_ids
    );
}

#[test]
fn share_seq_does_not_surface_bulk_rnaseq_companion() {
    let prompt = "SHARE-seq on adult mouse skin tissue — combinatorial \
                  split-pool barcoding, giving paired single-nucleus RNA \
                  and ATAC from each nucleus through the shared barcode. \
                  Joint embedding, clustering, cell-type annotation, \
                  differential expression per cluster.";
    let r = classifier().classify(prompt);
    let companion_ids: Vec<&str> = r
        .additional_modalities
        .iter()
        .map(|m| m.modality.as_str())
        .collect();
    assert!(
        companion_ids.contains(&"atac_seq"),
        "atac_seq must be a companion; got {:?}",
        companion_ids
    );
    assert!(
        !companion_ids.contains(&"bulk_rnaseq"),
        "bulk_rnaseq must NOT be a companion on SHARE-seq prompt; got {:?}",
        companion_ids
    );
}

#[test]
fn diablo_keeps_bulk_rnaseq_companion() {
    // Inverse guardrail: DIABLO genuinely co-implies bulk_rnaseq +
    // proteomics — the override MUST still lift the
    // (bulk_rnaseq, proteomics) suppression when DIABLO is named.
    let prompt = "Paired bulk RNA-seq and mass-spec proteomics on a \
                  breast-cancer tumor cohort. We want a supervised \
                  cross-omics integrator (sparse PLS-DA / DIABLO).";
    let r = classifier().classify(prompt);
    let all_modalities: Vec<&str> = std::iter::once(r.modality.as_str())
        .chain(r.additional_modalities.iter().map(|m| m.modality.as_str()))
        .collect();
    assert!(
        all_modalities.contains(&"bulk_rnaseq") && all_modalities.contains(&"proteomics"),
        "DIABLO prompt must surface bulk_rnaseq + proteomics; got {:?}",
        all_modalities
    );
}
