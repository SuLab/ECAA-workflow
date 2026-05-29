//! Spec §7.3 / §7.4 — RO-Crate emission registers literature evidence
//! as CreativeWork/ScholarlyArticle entities, wires prov:wasDerivedFrom
//! from CSVs to their source artifacts, and strips redistributable=false
//! content from the shareable export while preserving its metadata.

use ecaa_workflow_conversation::emit::{emit_ro_crate, emit_ro_crate_shareable};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

/// Build a minimal package with one literature output directory.
/// Two evidence entries — one redistributable, one not.
/// Writes a stub `ro-crate-metadata.json` with the minimal required shape.
fn fixture_package_with_literature(dir: &Path) {
    let task_dir = dir.join("runtime/outputs/review_prior_work");
    let evidence_dir = task_dir.join("evidence");
    let local_only_dir = evidence_dir.join("_local_only");
    fs::create_dir_all(&local_only_dir).unwrap();

    fs::write(
        task_dir.join("prior_claims_matrix.csv"),
        "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\n\
         ACAN,gene,28123456,disc tissue reduction,0,pmc_oa_full_text,sha256:aa,2026-05-14T00:00:00Z,true,true\n\
         MYC,gene,28123458,proprietary quote,0,external_pdf_local_only,sha256:bb,2026-05-14T00:00:00Z,false,true\n",
    )
    .unwrap();

    fs::write(
        evidence_dir.join("manifest.json"),
        r#"{"schema_version":1,"entries":[
{"pmid":"28123456","source_kind":"pmc_oa_full_text","path":"28123456.xml",
 "sha256_binary":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
 "sha256_extracted_text":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
 "extracted_text_normalization":"collapse_whitespace_lowercase_v1",
 "bytes":12,"retrieval_ts":"2026-05-14T00:00:00Z","retrieval_query_id":"q001",
 "redistributable":true,"license":"CC-BY-4.0"},
{"pmid":"28123458","source_kind":"external_pdf_local_only","path":"_local_only/28123458.pdf",
 "sha256_binary":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
 "sha256_extracted_text":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
 "extracted_text_normalization":"collapse_whitespace_lowercase_v1",
 "bytes":34,"retrieval_ts":"2026-05-14T00:00:00Z","retrieval_query_id":"q002",
 "redistributable":false,"license":"proprietary_institutional_access"}
]}"#,
    )
    .unwrap();

    fs::write(
        evidence_dir.join("28123456.xml"),
        "disc tissue reduction observed",
    )
    .unwrap();
    fs::write(
        local_only_dir.join("28123458.pdf"),
        "proprietary quote text",
    )
    .unwrap();

    // Minimal ro-crate-metadata.json — the emitter reads + patches this.
    fs::write(
        dir.join("ro-crate-metadata.json"),
        r#"{
  "@context": "https://w3id.org/ro/crate/1.1/context",
  "@graph": [
    {
      "@id": "ro-crate-metadata.json",
      "@type": "CreativeWork",
      "conformsTo": {"@id": "https://w3id.org/ro/crate/1.1"},
      "about": {"@id": "./"}
    },
    {
      "@id": "./",
      "@type": "Dataset",
      "name": "test package",
      "hasPart": [{"@id": "WORKFLOW.json"}]
    }
  ]
}"#,
    )
    .unwrap();
}

#[test]
fn evidence_artifacts_registered_as_creative_work() {
    let dir = TempDir::new().unwrap();
    fixture_package_with_literature(dir.path());

    let crate_json = emit_ro_crate(dir.path()).expect("emit_ro_crate should succeed");
    let body = serde_json::to_string(&crate_json).unwrap();

    assert!(
        body.contains("\"PMID:28123456\""),
        "expected PMID:28123456 identifier in JSON-LD; body excerpt:\n{}",
        &body[..body.len().min(2000)]
    );
    assert!(
        body.contains("ScholarlyArticle"),
        "expected ScholarlyArticle @type; body:\n{}",
        &body[..body.len().min(2000)]
    );

    // Both entries are present.
    assert!(body.contains("\"PMID:28123456\""), "PMID 28123456 missing");
    assert!(body.contains("\"PMID:28123458\""), "PMID 28123458 missing");

    // License is normalised for CC entries.
    assert!(
        body.contains("creativecommons.org/licenses/by/4.0"),
        "CC-BY-4.0 should be normalised to a URI"
    );

    // Digests are present.
    assert!(body.contains("sha256:aaaa"), "sha256_binary digest missing");
    assert!(
        body.contains("sha256:cccc"),
        "sha256_extracted_text digest missing"
    );

    // Source kind + redistributable + retrieval ts.
    assert!(body.contains("pmc_oa_full_text"), "source_kind missing");
    assert!(
        body.contains("ecaax:redistributable"),
        "redistributable field missing"
    );
    assert!(
        body.contains("2026-05-14T00:00:00Z"),
        "retrieval timestamp missing"
    );

    // sameAs PubMed URL.
    assert!(
        body.contains("pubmed.ncbi.nlm.nih.gov/28123456"),
        "sameAs PubMed URL missing"
    );
}

#[test]
fn prov_was_derived_from_links_csv_to_evidence() {
    let dir = TempDir::new().unwrap();
    fixture_package_with_literature(dir.path());

    let crate_json = emit_ro_crate(dir.path()).expect("emit_ro_crate should succeed");
    let body = serde_json::to_string(&crate_json).unwrap();

    // The CSV node is registered.
    assert!(
        body.contains("prior_claims_matrix.csv"),
        "prior_claims_matrix.csv not registered"
    );
    // prov:wasDerivedFrom is present on the CSV node.
    assert!(
        body.contains("prov:wasDerivedFrom"),
        "prov:wasDerivedFrom missing from CSV node"
    );

    // Verify the graph structure more precisely.
    let graph = crate_json["@graph"].as_array().expect("@graph array");
    let csv_node = graph
        .iter()
        .find(|e| {
            e.get("@id")
                .and_then(|v| v.as_str())
                .map(|s| s.ends_with("prior_claims_matrix.csv"))
                .unwrap_or(false)
        })
        .expect("prior_claims_matrix.csv node must be in @graph");

    let derived_from = csv_node
        .get("prov:wasDerivedFrom")
        .expect("prov:wasDerivedFrom must exist on CSV node");

    // Must be an array of @id references pointing at evidence files.
    let refs = derived_from
        .as_array()
        .expect("prov:wasDerivedFrom should be an array");
    assert_eq!(refs.len(), 2, "should reference both evidence entries");

    let ids: Vec<&str> = refs
        .iter()
        .filter_map(|r| r.get("@id").and_then(|v| v.as_str()))
        .collect();
    assert!(
        ids.iter().any(|id| id.contains("28123456.xml")),
        "28123456.xml not in prov:wasDerivedFrom; ids={:?}",
        ids
    );
    assert!(
        ids.iter().any(|id| id.contains("28123458.pdf")),
        "28123458.pdf not in prov:wasDerivedFrom; ids={:?}",
        ids
    );
}

#[test]
fn shareable_export_omits_non_redistributable_content_but_keeps_metadata() {
    let dir = TempDir::new().unwrap();
    fixture_package_with_literature(dir.path());

    let crate_json =
        emit_ro_crate_shareable(dir.path()).expect("emit_ro_crate_shareable should succeed");
    let body = serde_json::to_string(&crate_json).unwrap();

    // Redistributable=false entry's metadata is PRESERVED.
    assert!(
        body.contains("\"PMID:28123458\""),
        "non-redistributable PMID:28123458 metadata must be preserved in shareable crate"
    );

    // Must carry the contentOmittedFromExport marker.
    assert!(
        body.contains("ecaax:contentOmittedFromExport"),
        "ecaax:contentOmittedFromExport missing on non-redistributable entry"
    );

    // Verify the marker is true on the correct entry.
    let graph = crate_json["@graph"].as_array().expect("@graph array");
    let non_redist_node = graph
        .iter()
        .find(|e| e.get("identifier").and_then(|v| v.as_str()) == Some("PMID:28123458"))
        .expect("PMID:28123458 node must be in @graph");

    assert_eq!(
        non_redist_node.get("ecaax:contentOmittedFromExport"),
        Some(&serde_json::Value::Bool(true)),
        "ecaax:contentOmittedFromExport must be true on non-redistributable entry"
    );

    // Redistributable=true entry must NOT have the omission marker.
    let redist_node = graph
        .iter()
        .find(|e| e.get("identifier").and_then(|v| v.as_str()) == Some("PMID:28123456"))
        .expect("PMID:28123456 node must be in @graph");

    assert!(
        redist_node.get("ecaax:contentOmittedFromExport").is_none(),
        "redistributable entry must NOT have ecaax:contentOmittedFromExport"
    );
}

#[test]
fn emit_ro_crate_is_idempotent() {
    let dir = TempDir::new().unwrap();
    fixture_package_with_literature(dir.path());

    // First call.
    let first = emit_ro_crate(dir.path()).unwrap();
    // Write back so the second call reads the updated file.
    fs::write(
        dir.path().join("ro-crate-metadata.json"),
        serde_json::to_vec_pretty(&first).unwrap(),
    )
    .unwrap();

    // Second call — must not duplicate nodes.
    let second = emit_ro_crate(dir.path()).unwrap();
    let graph = second["@graph"].as_array().expect("@graph");

    let pmid_count = graph
        .iter()
        .filter(|e| e.get("identifier").and_then(|v| v.as_str()) == Some("PMID:28123456"))
        .count();
    assert_eq!(
        pmid_count, 1,
        "PMID:28123456 node must appear exactly once after idempotent re-emit"
    );
}
