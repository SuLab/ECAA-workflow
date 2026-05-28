//! Conventional emission mode — Arm B″ control package.
//!
//! Emits a competent conventional-documentation envelope:
//! README.md + analysis.ipynb + ro-crate-metadata.json (basic) +
//! per-table CSVs. NO ECAA-specific sidecars.
//!
//! This routes around the full emit pipeline at the top of
//! `emit_with_conversation_log` when `Config::ecaa_mode ==
//! EcaaMode::Conventional`. The shape is intentionally minimal: it's
//! the "competent conventional documentation" control surface against
//! which Arms A / C1 / C2 / D are compared in the Aim 3A benchmark.
//!
//! What this emitter *does not* write (and Arm A does):
//! - `runtime/audit-proof-report.json`
//! - `runtime/decisions.jsonl`
//! - `runtime/claim-verification.json`
//! - `runtime/determinism-shim.json`
//! - `runtime/security-policy.json`
//! - `runtime/model-policy.json`
//! - `runtime/proofs.jsonl` / `runtime/assumptions.jsonl`
//! - `runtime/figure-diff.json` / `runtime/cross_version_diff.json`
//! - Any Tier-3 WRROC / P-PLAN / OPMW provenance markup
//!
//! The RO-Crate descriptor written here conforms to RO-Crate 1.1 only —
//! enough for a reader to discover the README + notebook + tables but
//! deliberately not enough to make a re-execution claim.

use anyhow::{Context, Result};
use serde_json::json;
use std::path::Path;
use tracing::instrument;

/// Emit a conventional-documentation envelope into `package_root`.
///
/// `intent_summary` is dropped verbatim into the README narrative and
/// notebook header. `tables` is a slice of `(filename, csv_contents)`
/// pairs written under `package_root/tables/`.
#[instrument(
    skip(intent_summary, tables),
    fields(
        package_root = %package_root.display(),
        table_count = tables.len()
    )
)]
pub fn emit_conventional(
    package_root: &Path,
    intent_summary: &str,
    tables: &[(&str, &str)],
) -> Result<()> {
    std::fs::create_dir_all(package_root)
        .with_context(|| format!("creating package root {}", package_root.display()))?;

    // README.md — narrative summary.
    let readme = format!(
        "# Analysis package\n\n## Intent\n\n{intent_summary}\n\n## Files\n\n\
         - `analysis.ipynb` — analysis notebook\n\
         - `ro-crate-metadata.json` — RO-Crate descriptor\n\
         - `tables/*.csv` — result tables\n\n\
         ## Methods\n\n[Detailed methods would be authored by the analyst.]\n"
    );
    std::fs::write(package_root.join("README.md"), readme).context("write README.md")?;

    // analysis.ipynb — minimal Jupyter scaffold.
    let ipynb = json!({
        "cells": [
            {
                "cell_type": "markdown",
                "metadata": {},
                "source": ["# Analysis\n\n", intent_summary]
            },
            {
                "cell_type": "code",
                "execution_count": null,
                "metadata": {},
                "outputs": [],
                "source": [
                    "# Load tables and run analysis\n",
                    "import pandas as pd\n"
                ]
            }
        ],
        "metadata": {
            "kernelspec": {
                "display_name": "Python 3",
                "language": "python",
                "name": "python3"
            }
        },
        "nbformat": 4,
        "nbformat_minor": 5
    });
    std::fs::write(
        package_root.join("analysis.ipynb"),
        serde_json::to_string_pretty(&ipynb)?,
    )
    .context("write analysis.ipynb")?;

    // ro-crate-metadata.json — basic RO-Crate 1.1 descriptor (NOT Tier-3 WRROC).
    let rocrate = json!({
        "@context": "https://w3id.org/ro/crate/1.1/context",
        "@graph": [
            {
                "@id": "ro-crate-metadata.json",
                "@type": "CreativeWork",
                "about": {"@id": "./"},
                "conformsTo": {"@id": "https://w3id.org/ro/crate/1.1"}
            },
            {
                "@id": "./",
                "@type": "Dataset",
                "name": "Conventional analysis package",
                "description": intent_summary,
                "hasPart": [
                    {"@id": "README.md"},
                    {"@id": "analysis.ipynb"}
                ]
            },
            {
                "@id": "README.md",
                "@type": "File",
                "encodingFormat": "text/markdown"
            },
            {
                "@id": "analysis.ipynb",
                "@type": "File",
                "encodingFormat": "application/x-ipynb+json"
            }
        ]
    });
    std::fs::write(
        package_root.join("ro-crate-metadata.json"),
        serde_json::to_string_pretty(&rocrate)?,
    )
    .context("write ro-crate-metadata.json")?;

    // Per-table CSVs.
    let tables_dir = package_root.join("tables");
    std::fs::create_dir_all(&tables_dir)
        .with_context(|| format!("creating tables dir {}", tables_dir.display()))?;
    for (name, csv) in tables {
        std::fs::write(tables_dir.join(name), csv)
            .with_context(|| format!("write table CSV {name}"))?;
    }

    Ok(())
}
