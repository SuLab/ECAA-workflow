//! R-24 property test: the cross-version diff function satisfies its
//! key algebraic properties:
//!
//! 1. **Identity** — `diff(A, A) == empty` (no Discordant rows, every
//!    row is Robust or Concordant). A package diffed against itself
//!    must reflect 100% concordance.
//!
//! 2. **Anti-symmetry of row classification** — swapping parent and
//!    child swaps `NewInChild` ↔ `DroppedInParent`, leaves Robust /
//!    Concordant / Discordant counts unchanged. The diff is not strictly
//!    "symmetric" (overall_concordance is direction-independent but the
//!    per-row labels carry direction), so we assert the structural
//!    swap.
//!
//! Generates two fake WRROC `results/tables/` directories under
//! `tempdir()` with a small randomized de.tsv table; runs
//! `diff_packages` and checks the invariants.

use ecaa_workflow_core::cross_version_diff::{
    diff_packages, CrossVersionConfig, RowClassification, TableDiffConfig,
};
use proptest::prelude::*;
use std::path::Path;

/// Strategy for one de.tsv row: gene id + log2FC + padj. Genes drawn
/// from a fixed pool so cross-version overlap is non-trivial.
fn arb_row() -> impl Strategy<Value = (String, f64, f64)> {
    (
        prop::sample::select(vec![
            "GENE_A", "GENE_B", "GENE_C", "GENE_D", "GENE_E", "GENE_F",
        ]),
        -5.0f64..5.0,
        0.0f64..1.0,
    )
        .prop_map(|(g, l, p)| (g.to_string(), l, p))
}

/// Generate a small de.tsv table (3-8 rows). Rows may repeat the same
/// gene; the diff engine collapses on lowercased entity, so the test
/// dedupes implicitly.
fn arb_table() -> impl Strategy<Value = Vec<(String, f64, f64)>> {
    prop::collection::vec(arb_row(), 3..8)
}

/// Materialize `rows` as a `results/tables/de.tsv` under `pkg_root` and
/// return the package root.
fn write_pkg(pkg_root: &Path, rows: &[(String, f64, f64)]) {
    let tables = pkg_root.join("results").join("tables");
    std::fs::create_dir_all(&tables).expect("create tables/");
    let mut body = String::from("gene\tlog2FC\tpadj\n");
    for (g, l, p) in rows {
        body.push_str(&format!("{g}\t{l}\t{p}\n"));
    }
    std::fs::write(tables.join("de.tsv"), body).expect("write de.tsv");
}

fn diff_config() -> CrossVersionConfig {
    CrossVersionConfig {
        tables: vec![TableDiffConfig {
            table_name: "de.tsv".into(),
            entity_column: "gene".into(),
            effect_size_column: "log2FC".into(),
            pvalue_raw_column: None,
            pvalue_adjusted_column: Some("padj".into()),
            significance_threshold: 0.05,
        }],
    }
}

proptest! {
    /// Identity: diff(A, A) must classify every row as Robust or
    /// Concordant (no Discordant, no NewInChild, no DroppedInParent).
    #[test]
    fn identity_self_diff_is_empty(rows in arb_table()) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pkg_a = tmp.path().join("a");
        let pkg_b = tmp.path().join("b");
        write_pkg(&pkg_a, &rows);
        write_pkg(&pkg_b, &rows);

        let report = diff_packages(&pkg_a, &pkg_b, &diff_config())
            .expect("diff_packages on identity dirs");
        prop_assert_eq!(report.tables.len(), 1);
        let table = &report.tables[0];
        prop_assert_eq!(table.n_discordant, 0);
        for row in &table.rows {
            prop_assert!(
                matches!(
                    row.classification,
                    RowClassification::Robust | RowClassification::Concordant
                ),
                "identity diff classified {:?} as {:?}",
                row.entity,
                row.classification
            );
        }
        prop_assert!((report.overall_concordance - 1.0).abs() < 1e-9);
    }

    /// Anti-symmetric row classification: swap(parent, child) swaps
    /// NewInChild ↔ DroppedInParent and leaves Robust / Concordant /
    /// Discordant counts unchanged.
    #[test]
    fn swap_reverses_new_and_dropped(
        parent_rows in arb_table(),
        child_rows in arb_table(),
    ) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pkg_p = tmp.path().join("parent");
        let pkg_c = tmp.path().join("child");
        write_pkg(&pkg_p, &parent_rows);
        write_pkg(&pkg_c, &child_rows);

        let forward = diff_packages(&pkg_p, &pkg_c, &diff_config())
            .expect("diff_packages forward");
        let reverse = diff_packages(&pkg_c, &pkg_p, &diff_config())
            .expect("diff_packages reverse");

        prop_assert_eq!(forward.tables.len(), 1);
        prop_assert_eq!(reverse.tables.len(), 1);
        let f = &forward.tables[0];
        let r = &reverse.tables[0];

        // Robust / Concordant / Discordant are direction-independent.
        prop_assert_eq!(f.n_robust, r.n_robust);
        prop_assert_eq!(f.n_concordant, r.n_concordant);
        prop_assert_eq!(f.n_discordant, r.n_discordant);
        prop_assert_eq!(f.n_overlap, r.n_overlap);

        // NewInChild ↔ DroppedInParent must swap.
        let f_new = f.rows.iter().filter(|r| {
            matches!(r.classification, RowClassification::NewInChild)
        }).count();
        let r_dropped = r.rows.iter().filter(|r| {
            matches!(r.classification, RowClassification::DroppedInParent)
        }).count();
        prop_assert_eq!(f_new, r_dropped);

        let f_dropped = f.rows.iter().filter(|r| {
            matches!(r.classification, RowClassification::DroppedInParent)
        }).count();
        let r_new = r.rows.iter().filter(|r| {
            matches!(r.classification, RowClassification::NewInChild)
        }).count();
        prop_assert_eq!(f_dropped, r_new);
    }
}
