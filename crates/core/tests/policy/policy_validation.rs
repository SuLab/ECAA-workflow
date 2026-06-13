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

/// The set of `assertion_type` values the harness's `run_assertion`
/// (crates/harness/src/main.rs) actually implements. Kept here as the
/// cross-crate guard because the test crate (core) can't link the harness
/// binary's private `run_assertion`. CRITICAL: `run_assertion` ends with
/// `_ => false` (fail-closed), so a contract that names an assertion_type the
/// harness does NOT implement would silently fail EVERY task that stage gates
/// (the json_key_equals trap: it is in the schema enum but unimplemented).
/// This list must stay in lockstep with the `match atype` arms in
/// `run_assertion`. `json_key_equals` is deliberately EXCLUDED — it is a schema
/// enum value with no implementing arm; any contract using it would fail
/// closed, so the assertion below treats it as unimplemented.
const HARNESS_IMPLEMENTED_ASSERTION_TYPES: &[&str] = &[
    "artifact_present",
    "artifact_non_empty_table",
    "artifact_glob_any",
    "string_contains",
    "numeric_threshold",
    "numeric_distribution",
    "reference_range_outlier",
    "positive_control_present",
    "negative_control_present",
    "cross_stage_output_comparison",
    "cross_field_equals",
    "formula_references_covariates",
];

/// Collect every distinct `assertion_type` used across a validation contract's
/// stages.
fn assertion_types_in_contract(contract: &serde_json::Value) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    if let Some(stages) = contract.get("stages").and_then(|v| v.as_object()) {
        for block in stages.values() {
            if let Some(arr) = block.get("assertions").and_then(|v| v.as_array()) {
                for a in arr {
                    if let Some(t) = a.get("assertion_type").and_then(|v| v.as_str()) {
                        out.insert(t.to_string());
                    }
                }
            }
        }
    }
    out
}

/// The new DE/regression method-correctness contract must (1) validate against
/// its schema sidecar, and (2) use ONLY assertion_types the harness implements
/// in `run_assertion`. Guards the json_key_equals fail-closed trap: a contract
/// naming an unimplemented type would block every gated task silently because
/// `run_assertion`'s `_ => false` arm fails closed.
#[test]
fn association_contract_validates_and_uses_only_implemented_assertion_types() {
    let path = policies_dir().join("validation-contract-association.json");
    // (1) schema validation (also catches a missing sidecar).
    let contract = load_and_validate(&path)
        .unwrap_or_else(|e| panic!("association contract failed schema validation: {:#}", e));

    // (2) every assertion_type is implemented in the harness.
    let used = assertion_types_in_contract(&contract);
    assert!(
        !used.is_empty(),
        "association contract declares no assertions"
    );
    for t in &used {
        assert!(
            HARNESS_IMPLEMENTED_ASSERTION_TYPES.contains(&t.as_str()),
            "association contract uses assertion_type `{t}` which is NOT implemented in \
             run_assertion (it would fail closed via the `_ => false` arm — the \
             json_key_equals trap). Implement it or stop using it."
        );
    }
    // The two new method-correctness arms must actually appear (otherwise the
    // contract isn't doing its job).
    assert!(
        used.contains("cross_field_equals"),
        "association contract must use cross_field_equals (da-8-1 inversion catch)"
    );
    assert!(
        used.contains("formula_references_covariates"),
        "association contract must use formula_references_covariates (da-15-1 naked-design catch)"
    );
}

/// The single-cell contract's DE stage now carries the same two method-correctness
/// arms; assert it too only uses harness-implemented assertion_types (it routes
/// through the same fail-closed `run_assertion`).
#[test]
fn singlecell_contract_uses_only_implemented_assertion_types() {
    let path = policies_dir().join("validation-contract-singlecell.json");
    let contract = load_and_validate(&path)
        .unwrap_or_else(|e| panic!("singlecell contract failed schema validation: {:#}", e));
    for t in assertion_types_in_contract(&contract) {
        assert!(
            HARNESS_IMPLEMENTED_ASSERTION_TYPES.contains(&t.as_str()),
            "singlecell contract uses assertion_type `{t}` not implemented in run_assertion \
             (would fail closed)"
        );
    }
}

/// Helper: collect all assertions of a contract stage as (id, value) pairs.
fn stage_assertions<'a>(
    contract: &'a serde_json::Value,
    stage: &str,
) -> Vec<&'a serde_json::Value> {
    contract
        .get("stages")
        .and_then(|s| s.get(stage))
        .and_then(|b| b.get("assertions"))
        .and_then(|a| a.as_array())
        .map(|v| v.iter().collect())
        .unwrap_or_default()
}

/// Fail-open closure (gaming-audit hardening): the mtDNA heteroplasmy checks are
/// `when`-gated on /is_mtdna, so a result.json that OMITS is_mtdna would skip them
/// (skip-as-pass). Each variant stage that carries those gated checks must ALSO
/// carry a universal (un-gated) REQUIRED `is_mtdna_recorded` assertion so an
/// absent/tampered is_mtdna fails closed instead of silently dodging the het
/// checks. Guards against silently removing that closure.
#[test]
fn variant_contract_has_ungated_is_mtdna_recorded_guard() {
    let path = policies_dir().join("validation-contract-variants.json");
    let contract: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    for stage in ["variant_calling", "variant_filtering"] {
        let guard = stage_assertions(&contract, stage).into_iter().find(|a| {
            a.get("id").and_then(|v| v.as_str()) == Some(&format!("{stage}.is_mtdna_recorded"))
        });
        let g = guard.unwrap_or_else(|| {
            panic!("{stage} must carry an is_mtdna_recorded fail-open-closure assertion")
        });
        assert_eq!(
            g.get("severity").and_then(|v| v.as_str()),
            Some("required"),
            "{stage}.is_mtdna_recorded must be required"
        );
        assert!(
            g.get("when").is_none(),
            "{stage}.is_mtdna_recorded must be UN-gated (no `when`) — gating it would \
             reintroduce the fail-open it closes"
        );
        let subs = g
            .get("check")
            .and_then(|c| c.get("substrings"))
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|s| s.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            subs.contains(&"is_mtdna"),
            "{stage}.is_mtdna_recorded must require the is_mtdna field"
        );
    }
}

/// Self-report-evasion hardening (gaming-audit): the covariate-adjustment check
/// is `when`-gated on /available_covariates, so omitting it would skip the check.
/// The `design_recorded` precondition must REQUIRE available_covariates so it
/// cannot be omitted to dodge the covariate check.
#[test]
fn association_design_recorded_requires_available_covariates() {
    let path = policies_dir().join("validation-contract-association.json");
    let contract: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let recorded = stage_assertions(&contract, "differential_expression")
        .into_iter()
        .find(|a| {
            a.get("id").and_then(|v| v.as_str())
                == Some("differential_expression.design_recorded")
        })
        .expect("association contract must have a design_recorded precondition");
    let subs = recorded
        .get("check")
        .and_then(|c| c.get("substrings"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|s| s.as_str()).collect::<Vec<_>>())
        .unwrap_or_default();
    for required in ["design_formula", "response_variable", "available_covariates"] {
        assert!(
            subs.contains(&required),
            "design_recorded must require `{required}` (closing the omission false-negative)"
        );
    }
}

// The single-cell validation contract is carried by the GENERAL single_cell_de
// archetype, so its `required` assertions must be goal-agnostic. This guards
// against regressing the IVD-derivation anti-pattern: no required metadata
// column may be a study-specific clinical covariate, and no required artifact
// path may hardcode the Lotz `compartment_*` partitioning (a flat-layout
// single-cell run must still pass). Recursive `**` globs are how both layouts
// are matched.
#[test]
fn singlecell_contract_required_assertions_are_goal_agnostic() {
    let path = policies_dir().join("validation-contract-singlecell.json");
    let raw = fs::read_to_string(&path).expect("singlecell contract readable");
    let contract: serde_json::Value = serde_json::from_str(&raw).expect("contract parses");
    let stages = contract["stages"].as_object().expect("stages object");

    for (stage, block) in stages {
        for a in block["assertions"].as_array().expect("assertions array") {
            if a["severity"].as_str() != Some("required") {
                continue;
            }
            let id = a["id"].as_str().unwrap_or("<no id>");

            // (1) No required metadata-column check may demand a study-specific
            // clinical covariate. Only a sample-level join key is universal.
            if let Some(check) = a.get("check") {
                let needles = check
                    .get("substrings")
                    .or_else(|| check.get("substrings_any"))
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|s| s.as_str())
                            .map(str::to_lowercase)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                for n in &needles {
                    for banned in ["pfirrmann", "compartment", "age_years", "ivd_score"] {
                        assert!(
                            !n.contains(banned),
                            "required assertion `{id}` (stage {stage}) demands study-specific \
                             column `{n}` (contains `{banned}`) — would false-fail a general \
                             single-cell study; only a sample join key may be required"
                        );
                    }
                }
            }

            // (2) No required artifact path may hardcode `compartment_*`/`compartment_NP`.
            if let Some(target) = a.get("target").and_then(|v| v.as_str()) {
                assert!(
                    !target.contains("compartment_"),
                    "required assertion `{id}` (stage {stage}) hardcodes a `compartment_*` path \
                     (`{target}`) — use a recursive `**` glob so a flat single-cell layout passes"
                );
            }
        }
    }
}
