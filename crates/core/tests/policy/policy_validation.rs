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
// `unverifiable` rows. `NES` (Normalized Enrichment Score) is now in that
// deny-list too — it collides with the Nestin gene symbol, and the statistic
// sense dominates genomics prose (see the in-test rationale). Unambiguous gene
// symbols (e.g. AKT1) must still extract.
#[test]
fn interpretation_policy_excludes_stats_acronyms_not_genes() {
    use ecaa_workflow_core::claim_extractor::{extract_claims, ExtractorConfig};

    let policy_path = policies_dir().join("interpretation-policy.json");
    let raw = fs::read_to_string(&policy_path).expect("interpretation policy readable");
    let policy: serde_json::Value = serde_json::from_str(&raw).expect("policy parses");
    let cfg = ExtractorConfig::from_policy(&policy).expect("extractor config builds");

    let text = "AKT1 was significantly upregulated at FDR < 0.05 using GSEA on \
                TPM-normalized counts (Table de). Adipogenesis was enriched (NES = 1.92).";
    let claims = extract_claims(text, &cfg);
    let entities: Vec<&str> = claims.iter().map(|c| c.entity.as_str()).collect();

    // `NES` is now deny-listed. The token collides: it is the HGNC symbol for
    // Nestin AND the standard abbreviation for the GSEA Normalized Enrichment
    // Score. In genomics results prose the statistic is overwhelmingly the
    // intended sense (every gene-set enrichment paragraph reports "NES = x"),
    // and treating it as the Nestin gene produced 6 guaranteed false-mismatch
    // verdicts in the Himes airway package — each "NES = <score>" bound to the
    // Nestin row of an unrelated DE/literature table (log2FC -0.78). The earlier
    // "must not over-reach, keep Nestin" stance is reversed: the recurring
    // statistic collision outweighs the rare bare-Nestin mention, which can
    // still be verified when written with its Ensembl id or full name.
    for acronym in [
        "FDR", "GSEA", "TPM", "CPM", "FPKM", "DE", "DEG", "FC", "QC", "NES", "FWER",
    ] {
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
    "cross_stage_table_handoff",
    "cross_field_equals",
    "formula_references_covariates",
    "json_pointer_is_bool",
    "json_pointer_is_array",
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

/// Bulk DE must consume the exact post-QC raw-count table and retain its
/// feature population. This protects the typed edge with an executable
/// artifact/provenance check, so an ancestor count matrix cannot substitute
/// for the direct QC handoff.
#[test]
fn association_contract_requires_canonical_count_handoff() {
    let path = policies_dir().join("validation-contract-association.json");
    let contract = load_and_validate(&path)
        .unwrap_or_else(|e| panic!("association contract failed schema validation: {e:#}"));
    let handoff = stage_assertions(&contract, "differential_expression")
        .into_iter()
        .find(|a| {
            a.get("id").and_then(|v| v.as_str())
                == Some("differential_expression.canonical_count_handoff")
        })
        .expect("association contract must require the canonical QC-to-DE handoff");
    assert_eq!(
        handoff.get("assertion_type").and_then(|v| v.as_str()),
        Some("cross_stage_table_handoff")
    );
    let check = handoff.get("check").expect("handoff check present");
    assert_eq!(
        check.get("upstream_task").and_then(|v| v.as_str()),
        Some("qc_preprocessing")
    );
    assert_eq!(
        check.get("upstream_file").and_then(|v| v.as_str()),
        Some("filtered_count_matrix.tsv")
    );
    assert_eq!(
        check.get("declared_port").and_then(|v| v.as_str()),
        Some("raw_counts")
    );
    assert_eq!(
        check
            .get("alternative_ports")
            .and_then(|v| v.as_array())
            .and_then(|ports| ports.first())
            .and_then(|v| v.as_str()),
        Some("normalized_counts")
    );
    assert_eq!(
        handoff.get("severity").and_then(|v| v.as_str()),
        Some("required")
    );
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

/// Fail-open closure (tautology hardening): the mtDNA heteroplasmy checks are
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
        // The guard must use the TYPED json_pointer_is_bool check (not a raw-bytes
        // substring): a substring match is satisfied by any incidental occurrence
        // of the field name in a note/string while /is_mtdna never resolves to a
        // bool — the very fail-open this guard exists to close.
        assert_eq!(
            g.get("assertion_type").and_then(|v| v.as_str()),
            Some("json_pointer_is_bool"),
            "{stage}.is_mtdna_recorded must use the typed json_pointer_is_bool check, \
             not a substring (which an incidental field-name occurrence would satisfy)"
        );
        assert_eq!(
            g.get("check")
                .and_then(|c| c.get("json_pointer"))
                .and_then(|v| v.as_str()),
            Some("/is_mtdna"),
            "{stage}.is_mtdna_recorded must verify the /is_mtdna pointer resolves to a bool"
        );
    }
}

/// Self-report-evasion hardening: the covariate-adjustment check
/// is `when`-gated on the JSON pointer /available_covariates, so omitting it (or
/// recording it at a nested key / inside a free-text note) would skip the check.
/// Both DE contracts must carry a `design_records_covariate_columns` assertion
/// that REQUIRES /available_covariates to resolve to an ARRAY via the SAME
/// pointer read-mechanism the adjustment check's `when` gate uses — closing the
/// substring-vs-pointer mismatch a plain `string_contains` left open. The check
/// must be `required`, un-gated (so absence fails closed), and typed
/// `json_pointer_is_array` (an empty array is allowed; the adjustment check's own
/// empty-array `when` gate then self-skips). Shared by both contracts.
#[test]
fn de_contracts_record_covariate_columns_via_typed_pointer() {
    for file in [
        "validation-contract-association.json",
        "validation-contract-singlecell.json",
    ] {
        let path = policies_dir().join(file);
        let contract: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let guard = stage_assertions(&contract, "differential_expression")
            .into_iter()
            .find(|a| {
                a.get("id").and_then(|v| v.as_str())
                    == Some("differential_expression.design_records_covariate_columns")
            })
            .unwrap_or_else(|| {
                panic!("{file}: must carry a design_records_covariate_columns assertion")
            });
        assert_eq!(
            guard.get("assertion_type").and_then(|v| v.as_str()),
            Some("json_pointer_is_array"),
            "{file}: covariate-columns guard must use the TYPED json_pointer_is_array check \
             (the same pointer read the adjustment when-gate uses), not a substring"
        );
        assert_eq!(
            guard
                .get("check")
                .and_then(|c| c.get("json_pointer"))
                .and_then(|v| v.as_str()),
            Some("/available_covariates"),
            "{file}: covariate-columns guard must verify /available_covariates"
        );
        assert_eq!(
            guard.get("severity").and_then(|v| v.as_str()),
            Some("required"),
            "{file}: covariate-columns guard must be required"
        );
        assert!(
            guard.get("when").is_none(),
            "{file}: covariate-columns guard must be UN-gated so an absent \
             /available_covariates fails closed (the closed omission fail-open)"
        );
    }
}

/// Effect-size-reliability check (C5, da-15-1): both DE contracts must carry a
/// `differential_expression.top_effect_reliability` assertion that (1) is a
/// `numeric_threshold` `gte` on the agent-recomputed /top_effect_abundance_ratio
/// scalar (read from result.json) with the operator-authored floor 0.20, (2) is
/// `when`-gated on /information_column_recorded == true so a DE table with no
/// abundance column is SKIPPED (never false-failed) rather than fail-closing on
/// a missing metric, (3) is `required` severity (the harness only evaluates
/// required assertions — a `recommended` check is inert), and (4) names no
/// analysis method in its description (method neutrality). The ratio metric
/// replaces an earlier bottom-quartile COUNT that under-fired on the real
/// da-15-1 table (the full-table quartile cut sat below the low-count hits). The
/// ratio is null-robust — ≈1 under independence, ≈0 for the unshrunken-low-count
/// artifact — mirroring the variant het_tail_band_nonempty check
/// (recompute-vs-operator-bound, method-neutral). Guards against silently
/// dropping the check, regressing its self-skip gate, or demoting it back to a
/// non-enforced severity.
#[test]
fn de_contracts_carry_method_neutral_top_effect_reliability_check() {
    // Method tokens that must NEVER appear in the agent-facing description: it
    // states a property of the agent's own ranking, never a remedy.
    const METHOD_TOKENS: [&str; 10] = [
        "deseq",
        "edger",
        "limma",
        "voom",
        "apeglm",
        "ashr",
        "shrink",
        "wilcoxon",
        "mast",
        "set the threshold",
    ];
    for contract_file in [
        "validation-contract-association.json",
        "validation-contract-singlecell.json",
    ] {
        let path = policies_dir().join(contract_file);
        // load_and_validate runs the schema sidecar, so the new assertion must
        // also pass the contract's JSON Schema.
        let contract = load_and_validate(&path)
            .unwrap_or_else(|e| panic!("{contract_file} failed schema validation: {e:#}"));
        let a = stage_assertions(&contract, "differential_expression")
            .into_iter()
            .find(|a| {
                a.get("id").and_then(|v| v.as_str())
                    == Some("differential_expression.top_effect_reliability")
            })
            .unwrap_or_else(|| {
                panic!("{contract_file} must carry differential_expression.top_effect_reliability")
            });
        assert_eq!(
            a.get("assertion_type").and_then(|v| v.as_str()),
            Some("numeric_threshold"),
            "{contract_file}: top_effect_reliability must be a numeric_threshold"
        );
        let check = a.get("check").expect("check present");
        assert_eq!(
            check.get("json_pointer").and_then(|v| v.as_str()),
            Some("/top_effect_abundance_ratio"),
            "{contract_file}: must read the agent-recomputed /top_effect_abundance_ratio"
        );
        assert_eq!(
            check.get("op").and_then(|v| v.as_str()),
            Some("gte"),
            "{contract_file}: the top-effect abundance ratio must be bounded BELOW (a low ratio is the artifact)"
        );
        assert_eq!(
            check.get("value").and_then(|v| v.as_f64()),
            Some(0.20),
            "{contract_file}: operator-authored ratio floor must be 0.20"
        );
        // Enforcement fix: the harness only evaluates `required` assertions, so
        // a `recommended` severity left the check inert.
        assert_eq!(
            a.get("severity").and_then(|v| v.as_str()),
            Some("required"),
            "{contract_file}: top_effect_reliability must be required (the harness skips recommended)"
        );
        // Self-skip gate: when no abundance column was recorded, the check is
        // not applicable and must be skipped (the gate is the precondition, not
        // a prescribed output). Matches the het_tail_band_nonempty `equals: true`
        // shape so a recorded `false` reads as not-applicable.
        let when = a.get("when").expect("when gate present");
        assert_eq!(
            when.get("json_pointer").and_then(|v| v.as_str()),
            Some("/information_column_recorded"),
            "{contract_file}: must self-skip via the /information_column_recorded gate"
        );
        assert_eq!(
            when.get("equals"),
            Some(&serde_json::json!(true)),
            "{contract_file}: gate must require information_column_recorded == true"
        );
        // Method neutrality: the agent-facing description states a property of
        // the agent's own ranking and prescribes no analysis method.
        let desc = a
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        for token in METHOD_TOKENS {
            assert!(
                !desc.contains(token),
                "{contract_file}: top_effect_reliability description leaked method token {token:?}"
            );
        }
    }
}

/// Report-completeness checks (da-8-1 C8): both DE contracts must carry
/// `differential_expression.reports_model_fit` and
/// `differential_expression.reports_per_row_n`. Each must (1) be a
/// `string_contains` (no new assertion_type), (2) read the agent's OWN folded
/// narrative via `check.json_pointer == /narrative_text` so the search is scoped
/// off the field names (a whole-file search would match `r_squared` inside the
/// flag key `r_squared_column_recorded` and false-pass), (3) use `substrings_any`
/// (OR) so any reasonable surfacing matches — never `substrings` (AND), which
/// would demand a specific phrasing, (4) be `when`-gated on the corresponding
/// presence flag with `equals: true` so a table that did NOT record the column is
/// SKIPPED (never blocks, never prescribes producing the column), (5) be
/// `required` severity (the harness skips `recommended`), and (6) name NO
/// analysis method, estimator, threshold value, gene name, task id, or
/// benchmark-specific column literal in the description or in any check clause
/// (method neutrality + no eval-overfit). Guards against silently dropping the
/// checks, regressing the scoping/gate, demoting severity, or leaking a method
/// token.
#[test]
fn de_contracts_carry_method_neutral_report_completeness_checks() {
    // Tokens that must NEVER appear in the agent-facing description (methods /
    // estimators) NOR any benchmark-specific value/literal (eval-overfit). The
    // checks state a fact about the agent's own recorded output, never a method
    // or a benchmark value. "41" is the da-8-1 passing-set count the DECLINED
    // half (b) would have read; it must never appear in any shipped clause.
    const BANNED_TOKENS: [&str; 12] = [
        "deseq",
        "edger",
        "limma",
        "voom",
        "wilcoxon",
        "mast",
        "shrink",
        "set the threshold",
        "41",
        "metabolite ~",
        "repurposing",
        "tier",
    ];
    let expected: [(&str, &str); 2] = [
        (
            "differential_expression.reports_model_fit",
            "/r_squared_column_recorded",
        ),
        (
            "differential_expression.reports_per_row_n",
            "/sample_size_column_recorded",
        ),
    ];
    for contract_file in [
        "validation-contract-association.json",
        "validation-contract-singlecell.json",
    ] {
        let path = policies_dir().join(contract_file);
        let contract = load_and_validate(&path)
            .unwrap_or_else(|e| panic!("{contract_file} failed schema validation: {e:#}"));
        for (id, gate_ptr) in expected {
            let a = stage_assertions(&contract, "differential_expression")
                .into_iter()
                .find(|a| a.get("id").and_then(|v| v.as_str()) == Some(id))
                .unwrap_or_else(|| panic!("{contract_file} must carry {id}"));
            // (1) string_contains, no new assertion_type.
            assert_eq!(
                a.get("assertion_type").and_then(|v| v.as_str()),
                Some("string_contains"),
                "{contract_file}: {id} must be a string_contains (no new assertion_type)"
            );
            let check = a.get("check").expect("check present");
            // (2) scoped to /narrative_text (the folded agent narrative channel).
            assert_eq!(
                check.get("json_pointer").and_then(|v| v.as_str()),
                Some("/narrative_text"),
                "{contract_file}: {id} must scope the search to /narrative_text \
                 (a whole-file search would false-pass on the field name)"
            );
            // (3) substrings_any (OR), never substrings (AND).
            assert!(
                check
                    .get("substrings_any")
                    .and_then(|v| v.as_array())
                    .is_some(),
                "{contract_file}: {id} must use substrings_any (OR), not substrings (AND)"
            );
            assert!(
                check.get("substrings").is_none(),
                "{contract_file}: {id} must NOT use substrings (AND) — it would demand one phrasing"
            );
            // (4) gated on the presence flag with equals: true.
            let when = a.get("when").expect("when gate present");
            assert_eq!(
                when.get("json_pointer").and_then(|v| v.as_str()),
                Some(gate_ptr),
                "{contract_file}: {id} must self-skip via the {gate_ptr} presence gate"
            );
            assert_eq!(
                when.get("equals"),
                Some(&serde_json::json!(true)),
                "{contract_file}: {id} gate must require the presence flag == true"
            );
            // (5) required severity.
            assert_eq!(
                a.get("severity").and_then(|v| v.as_str()),
                Some("required"),
                "{contract_file}: {id} must be required (the harness skips recommended)"
            );
            // (6) neutrality + no-overfit: scan the description AND every check
            // clause string for a banned method/value token.
            let desc = a
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let check_blob = serde_json::to_string(check).unwrap().to_ascii_lowercase();
            for token in BANNED_TOKENS {
                assert!(
                    !desc.contains(token),
                    "{contract_file}: {id} description leaked banned token {token:?}"
                );
                assert!(
                    !check_blob.contains(token),
                    "{contract_file}: {id} check clause leaked banned token {token:?}"
                );
            }
        }
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
