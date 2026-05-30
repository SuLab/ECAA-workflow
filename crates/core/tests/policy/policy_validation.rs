//! End-to-end policy schema validation. For every `<name>.json` file
//! under `config/downstream-policy/`, two checks:
//!
//! 1. The live policy validates against its `<name>.schema.json` sidecar.
//! 2. A mutated copy missing the required `schemaVersion` field fails
//! validation with a message pointing at the offending path.

use ecaa_workflow_core::policy_schema::load_and_validate;
use std::fs;
use std::path::{Path, PathBuf};

fn policies_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/downstream-policy")
}

fn live_policies() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(policies_dir())
        .expect("policies dir readable")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or_default();
            // `_`-prefixed files are support/meta files (shared vocab,
            // policy skeleton schema) — not independently loadable policies.
            name.ends_with(".json") && !name.ends_with(".schema.json") && !name.starts_with('_')
        })
        .collect();
    out.sort();
    out
}

// Phase B4 — `document_policy_reference_footprint` deleted with the
// legacy `config/stage-taxonomies/` directory. The pre-B4 test scanned
// taxonomy YAMLs for `policies:` / `validation_contract_ref:`
// references to identify orphan downstream policies. With the YAMLs
// gone the equivalent check would need to scan archetype YAMLs;
// archetypes don't yet author per-archetype `policies` allowlists
// (they're populated via `policy_context::PolicyContext` at compose
// time instead). Re-introducing this coverage on v4 is out of scope
// for B4.

#[test]
fn all_live_policies_validate() {
    // Floor guards against accidental mass deletion. An upper bound
    // is intentionally not pinned — it would go stale every time a
    // new policy is added. The per-policy `load_and_validate` loop
    // below is the load-bearing check — it catches both
    // schema-violating Content and missing schema sidecars for every
    // policy.
    let policies = live_policies();
    assert!(
        policies.len() >= 10,
        "policies dir should contain at least the 10 foundational policies, found {}: {:?}",
        policies.len(),
        policies
    );
    for p in &policies {
        load_and_validate(p)
            .unwrap_or_else(|e| panic!("validation failed for {}: {:#}", p.display(), e));
    }
}

/// The shared skeleton catches a missing `schemaVersion` on every
/// claim-boundary policy, even when the policy-specific sidecar no
/// longer enforces the shared shape. Exercises the thin three
/// (trajectory + cell-communication + interpretation).
#[test]
fn shared_skeleton_catches_missing_schema_version_for_all_claim_boundary_policies() {
    // `trajectory-policy` +
    // `cell-communication-policy` were moved under archive/;
    // only `interpretation-policy` remains in the live claim-boundary
    // set. The skeleton check still has to exercise at least one domain
    // sidecar so a future regression in the shared skeleton surfaces.
    let stem = "interpretation-policy";
    let src = policies_dir().join(format!("{}.json", stem));
    let raw = fs::read_to_string(&src).expect("live policy readable");
    let mut v: serde_json::Value = serde_json::from_str(&raw).expect("live policy parseable");
    v.as_object_mut()
        .expect("top-level object")
        .remove("schemaVersion");

    let tmp = tempfile::tempdir().unwrap();
    let bad_path = tmp.path().join(format!("{}.json", stem));
    fs::write(&bad_path, serde_json::to_string(&v).unwrap()).unwrap();

    // Copy only the domain sidecar + skeleton — the point is that
    // removing the per-policy `schemaVersion` requirement from the
    // sidecar doesn't weaken validation because the
    // skeleton catches it.
    fs::copy(
        policies_dir().join(format!("{}.schema.json", stem)),
        tmp.path().join(format!("{}.schema.json", stem)),
    )
    .unwrap();
    fs::copy(
        policies_dir().join("_policy-skeleton.schema.json"),
        tmp.path().join("_policy-skeleton.schema.json"),
    )
    .unwrap();

    let err = load_and_validate(&bad_path).expect_err("skeleton should flag missing schemaVersion");
    let msg = format!("{:#}", err);
    assert!(
        msg.contains("schemaVersion") || msg.contains("\"required\"") || msg.contains("skeleton"),
        "expected skeleton violation on {}, got: {}",
        stem,
        msg
    );
}

#[test]
fn every_policy_has_a_schema_sidecar() {
    for p in live_policies() {
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap();
        let schema = policies_dir().join(format!("{}.schema.json", stem));
        assert!(
            schema.exists(),
            "missing schema sidecar {} for policy {}",
            schema.display(),
            p.display()
        );
    }
}

/// Per-policy negative test: drop the `schemaVersion` field and confirm
/// the validator flags it. One test per policy so a failure points at the
/// exact file and the CI output stays readable.
macro_rules! negative_test {
    ($name:ident, $stem:literal) => {
        #[test]
        fn $name() {
            let src = policies_dir().join(concat!($stem, ".json"));
            let raw = fs::read_to_string(&src).expect("live policy readable");
            let mut v: serde_json::Value =
                serde_json::from_str(&raw).expect("live policy parseable");
            let obj = v.as_object_mut().expect("top-level object");
            obj.remove("schemaVersion");

            let tmp = tempfile::tempdir().unwrap();
            let bad_path = tmp.path().join(concat!($stem, ".json"));
            fs::write(&bad_path, serde_json::to_string(&v).unwrap()).unwrap();

            // Copy the sidecar into the tmp dir so load_and_validate finds it.
            let schema_src = policies_dir().join(concat!($stem, ".schema.json"));
            let schema_dst = tmp.path().join(concat!($stem, ".schema.json"));
            fs::copy(&schema_src, &schema_dst).unwrap();
            // Some claim-boundary policies delegate the schemaVersion
            // requirement to `_policy-skeleton.schema.json`. Copy the
            // skeleton too when it exists so the validator finds it
            // alongside the policy-specific sidecar in the tmp dir.
            let skeleton_src = policies_dir().join("_policy-skeleton.schema.json");
            if skeleton_src.exists() {
                let skeleton_dst = tmp.path().join("_policy-skeleton.schema.json");
                fs::copy(&skeleton_src, &skeleton_dst).unwrap();
            }
            // Copy the shared vocab so $shared references resolve before
            // schema validation catches the missing schemaVersion field.
            let vocab_src = policies_dir().join("_shared-vocab.json");
            if vocab_src.exists() {
                fs::copy(&vocab_src, tmp.path().join("_shared-vocab.json")).unwrap();
            }

            let err = load_and_validate(&bad_path).expect_err("expected validation failure");
            let msg = format!("{:#}", err);
            assert!(
                msg.contains("schemaVersion") || msg.contains("\"required\""),
                "expected schemaVersion violation, got: {}",
                msg
            );
        }
    };
}

negative_test!(
    missing_schema_version_in_best_practice_evidence,
    "best-practice-evidence-policy"
);
negative_test!(
    missing_schema_version_in_best_practice_scoring,
    "best-practice-scoring-policy"
);
negative_test!(
    missing_schema_version_in_best_practice_validation_contract,
    "best-practice-validation-contract"
);
negative_test!(
    missing_schema_version_in_discovery_validation_contract,
    "discovery-validation-contract"
);
negative_test!(
    missing_schema_version_in_literature_grounding,
    "literature-grounding-policy"
);
negative_test!(
    missing_schema_version_in_source_discovery,
    "source-discovery-policy"
);
negative_test!(
    missing_schema_version_in_standards_and_repository,
    "standards-and-repository-policy"
);
negative_test!(
    missing_schema_version_in_interpretation,
    "interpretation-policy"
);
// `trajectory-policy`, `cell-communication-policy`,
// `best-practice-tool-registry`, `data-locator-resolution-policy`,
// and `retrieval-tool-registry` moved under archive/.
// No negative_test covers them — the emitter doesn't ship
// archive/.

// The shipped interpretation policy must exclude common statistics /
// method acronyms (FDR, GSEA, TPM, …) from the gene-symbol entity
// pattern. Without this, prose like "significant at FDR < 0.05 using
// GSEA on TPM counts" extracts FDR/GSEA/TPM as fabricated entities and
// pollutes every DE/enrichment report's verdict list with spurious
// `unverifiable` rows. Real gene symbols (e.g. NES = Nestin) must still
// extract.
#[test]
fn interpretation_policy_excludes_stats_acronyms_not_genes() {
    use ecaa_workflow_core::claim_extractor::{extract_claims, ExtractorConfig};

    let policy_path = policies_dir().join("interpretation-policy.json");
    let raw = fs::read_to_string(&policy_path).expect("interpretation policy readable");
    let policy: serde_json::Value = serde_json::from_str(&raw).expect("policy parses");
    let cfg = ExtractorConfig::from_policy(&policy).expect("extractor config builds");

    let text = "AKT1 was significantly upregulated at FDR < 0.05 using GSEA on \
                TPM-normalized counts (Table de). NES was also upregulated (Table de).";
    let claims = extract_claims(text, &cfg);
    let entities: Vec<&str> = claims.iter().map(|c| c.entity.as_str()).collect();

    for acronym in ["FDR", "GSEA", "TPM", "CPM", "FPKM", "DE", "DEG", "FC", "QC"] {
        assert!(
            !entities.contains(&acronym),
            "stats acronym `{}` must not be extracted as an entity (got {:?})",
            acronym,
            entities
        );
    }
    assert!(
        entities.contains(&"AKT1"),
        "real gene AKT1 must extract: {:?}",
        entities
    );
    assert!(
        entities.contains(&"NES"),
        "real gene NES (Nestin) must still extract — exclude list must not over-reach: {:?}",
        entities
    );
}
