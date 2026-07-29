//! Deterministic accountability ledger for every retained task output.

use crate::audit_proof::invariants::evidence_coverage::{
    coverage_scope, declaration_selects_output, reference_resolves_output,
};
use crate::audit_proof::loader::LoadedPackage;
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

/// Location of the machine-readable output ledger within a package.
pub(super) const OUTPUT_ACCOUNTABILITY_PATH: &str = "runtime/output-accountability.json";

#[derive(Debug, Serialize)]
struct OutputAccountabilityDocument {
    schema_version: &'static str,
    denominator_rule: &'static str,
    counts_by_role: BTreeMap<String, usize>,
    counts_by_disposition: BTreeMap<String, usize>,
    outputs: Vec<OutputAccountabilityEntry>,
}

#[derive(Debug, Serialize)]
struct OutputAccountabilityEntry {
    path: String,
    role: String,
    disposition: String,
    declared_as_claim_evidence: bool,
    claim_ids: Vec<String>,
}

fn refs_for_verdict(verdict: &Value) -> impl Iterator<Item = &str> {
    ["supported_by", "checked_against", "contradicts"]
        .into_iter()
        .filter_map(|field| verdict.get(field).and_then(Value::as_array))
        .flatten()
        .filter_map(Value::as_str)
}

fn claim_ids_for_output(pkg: &LoadedPackage, output: &str) -> Vec<String> {
    let mut ids = BTreeSet::new();
    let Some(verdicts) = pkg
        .claims
        .as_ref()
        .and_then(|claims| claims.get("verdicts"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    for verdict in verdicts {
        let linked =
            refs_for_verdict(verdict).any(|reference| reference_resolves_output(output, reference));
        if linked {
            if let Some(id) = verdict.get("claim_id").and_then(Value::as_str) {
                ids.insert(id.to_string());
            }
        }
    }
    ids.into_iter().collect()
}

fn unused_outputs(pkg: &LoadedPackage) -> BTreeSet<String> {
    pkg.assumptions
        .iter()
        .filter(|assumption| {
            assumption.get("kind").and_then(Value::as_str) == Some("output_unused")
        })
        .filter_map(|assumption| assumption.get("detail").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect()
}

/// Write a complete, deterministic ledger for every registered task output.
///
/// The evidence-coverage denominator remains narrow and explicit, while this
/// sidecar prevents the remaining retained outputs from becoming an
/// unexplained remainder. Each path receives one role and one disposition.
pub(super) fn write_output_accountability(root: &Path) -> Result<usize> {
    let pkg =
        LoadedPackage::from_root(root).context("loading package for output accountability")?;
    let scope = coverage_scope(&pkg);
    let unused = unused_outputs(&pkg);

    let mut by_path: BTreeMap<String, &'static str> = BTreeMap::new();
    for path in &scope.claim_evidence {
        by_path.insert(path.clone(), "claim_evidence");
    }
    for path in &scope.analytical_results {
        by_path.insert(path.clone(), "analytical_result");
    }
    for path in &scope.presentation {
        by_path.insert(path.clone(), "presentation");
    }
    for path in &scope.intermediate {
        by_path.insert(path.clone(), "intermediate");
    }
    for path in &scope.validation {
        by_path.insert(path.clone(), "validation");
    }
    for path in &scope.retained_inputs {
        by_path.insert(path.clone(), "retained_input");
    }
    for path in &scope.superseded {
        by_path.insert(path.clone(), "superseded");
    }
    for path in &scope.administrative {
        by_path.insert(path.clone(), "administrative");
    }

    let mut counts_by_role = BTreeMap::new();
    let mut counts_by_disposition = BTreeMap::new();
    let mut outputs = Vec::with_capacity(by_path.len());
    for (path, role) in by_path {
        let claim_ids = claim_ids_for_output(&pkg, &path);
        let declared_as_claim_evidence = pkg
            .declared_claim_evidence
            .iter()
            .any(|declaration| declaration_selects_output(&path, declaration));
        let disposition = if role == "superseded" {
            "superseded"
        } else if role == "claim_evidence" && !claim_ids.is_empty() {
            "used"
        } else if role == "claim_evidence"
            && unused
                .iter()
                .any(|item| reference_resolves_output(&path, item))
        {
            "intentionally_unused"
        } else if role == "claim_evidence" {
            "unaccounted"
        } else {
            "retained"
        };
        *counts_by_role.entry(role.to_string()).or_insert(0) += 1;
        *counts_by_disposition
            .entry(disposition.to_string())
            .or_insert(0) += 1;
        outputs.push(OutputAccountabilityEntry {
            path,
            role: role.to_string(),
            disposition: disposition.to_string(),
            declared_as_claim_evidence,
            claim_ids,
        });
    }

    let count = outputs.len();
    let document = OutputAccountabilityDocument {
        schema_version: "1",
        denominator_rule:
            "result_schema and report_schemas artifacts, plus resolved claim references",
        counts_by_role,
        counts_by_disposition,
        outputs,
    };
    let mut bytes =
        serde_json::to_vec_pretty(&document).context("serializing output accountability")?;
    bytes.push(b'\n');
    let path = root.join(OUTPUT_ACCOUNTABILITY_PATH);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ledger_accounts_for_every_output_and_flags_unlinked_declaration() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("runtime")).unwrap();
        fs::write(
            tmp.path().join("WORKFLOW.json"),
            serde_json::to_vec(&json!({
                "tasks": {
                    "assemble_report_data": {
                        "spec": {
                            "report_schemas": {
                                "de": {"artifact": "de.tsv"},
                                "pathway": {"artifact": "pathway.tsv"}
                            }
                        }
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            tmp.path().join("ro-crate-metadata.json"),
            serde_json::to_vec(&json!({
                "@graph": [
                    {"@id": "runtime/outputs/de/de.tsv", "@type": ["File", "Dataset"]},
                    {"@id": "runtime/outputs/pathway/pathway.tsv", "@type": ["File", "Dataset"]},
                    {"@id": "runtime/outputs/de/result.json", "@type": ["File", "Dataset"]}
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            tmp.path().join("runtime/claim-verification.json"),
            serde_json::to_vec(&json!({
                "verdicts": [{
                    "claim_id": "report#claim-1",
                    "status": "verified",
                    "supported_by": ["runtime/outputs/de/de.tsv"]
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(write_output_accountability(tmp.path()).unwrap(), 3);
        let document: Value =
            serde_json::from_slice(&fs::read(tmp.path().join(OUTPUT_ACCOUNTABILITY_PATH)).unwrap())
                .unwrap();
        assert_eq!(document["counts_by_role"]["claim_evidence"], 2);
        assert_eq!(document["counts_by_role"]["administrative"], 1);
        assert_eq!(document["counts_by_disposition"]["used"], 1);
        assert_eq!(document["counts_by_disposition"]["unaccounted"], 1);
        assert_eq!(document["counts_by_disposition"]["retained"], 1);
    }
}
