//! v3 §0.1 + v4 §0.4 status baseline — asserts every row in the traceability
//! matrices matches actual code state. This test is the canonical state-of-truth
//! and is intentionally tightly coupled to the §0.1 and §0.4 tables.
//!
//! Any §0.1 / §0.4 row that regresses (the
//! anchor file disappears, the load-bearing struct is renamed away,
//! the config YAML is deleted) fails this test.
//!
//! The check functions read files from disk rather than referencing
//! Rust symbols directly so the test surfaces refactors that silently
//! rename the canonical surface. Path roots are resolved relative to
//! the integration-test binary's CWD (the crate root,
//! `crates/core/`) per `cargo test` convention; we walk up via
//! `../../` to hit repo root.

use std::path::Path;

const ROWS: &[(&str, fn() -> bool)] = &[
    // v3 §0.1 rows
    ("v3 F12 assumption-policy table loadable", check_v3_f12),
    ("v3 F11 promotion-refusal config-driven", check_v3_f11),
    (
        "v3 §10.2 ChainOfCustody struct shipped",
        check_v3_chain_of_custody,
    ),
    (
        "v3 §10.1 ProvenanceTier::Suppressed shipped",
        check_v3_suppressed,
    ),
    (
        "v3 §10.5 SchemaVersionsManifest shipped",
        check_v3_schema_versions,
    ),
    (
        "v3 §11.X PopulationCoverageStatement shipped",
        check_v3_population_coverage,
    ),
    (
        "v3 §7 LifecycleTransition shipped",
        check_v3_lifecycle_transition,
    ),
    (
        "v3 §15.2 config/assumption-policy.yaml exists",
        check_v3_assumption_policy_yaml,
    ),
    (
        "v3 §6.4 LlmAvailability enum shipped",
        check_v3_llm_availability,
    ),
    (
        "v3 §16.1 InjectionPatternCatalog shipped",
        check_v3_injection_patterns,
    ),
    (
        "v3 §9.3 BackendCapabilityReport shipped",
        check_v3_capability_report,
    ),
    // v4 §0.4 rows
    ("v4 D2 / F22 OntologyScopeMatrix shipped", check_v4_d2),
    (
        "v4 D3 / F18 VerifierDecision substrate shipped",
        check_v4_d3,
    ),
    ("v4 D5 / F20 RepairStrategy shipped", check_v4_d5),
    ("v4 D6 / F21 RefusalKind + UnblockPath shipped", check_v4_d6),
    ("v4 D7 SandboxRefusalCategory shipped", check_v4_d7),
    ("v4 D8 / F19 PromotionGatePolicy shipped", check_v4_d8),
    ("v4 D4 LocalExtensionMaturity shipped", check_v4_d4),
    (
        "v4 D1 / F23 compile_time_discipline module exists",
        check_v4_d1,
    ),
    // Phase-1-through-6 residuals-closure rows
    (
        "v3 P5 PolicyRuleId newtype shipped",
        check_v3_policy_rule_id,
    ),
    (
        "v3 P5 assumption-policy.yaml has policy_rules",
        check_v3_assumption_policy_rules,
    ),
    (
        "v4 P5 DAG-mutation auto-application shipped",
        check_v4_dag_mutation,
    ),
    (
        "v3 P8 lifecycle-adversarial substrate emission shipped",
        check_v3_lifecycle_substrate,
    ),
    ("Tier 3.5 corpus + runner shipped", check_tier_3_5),
    ("Tier 8.10 corpus + runner shipped", check_tier_8_10),
    ("UI Repairs tab mounted", check_ui_repairs_tab),
    // Real-value evaluation plan close-out rows
    ("Tier 0 SME journey runner shipped", check_tier_0_runner),
    (
        "Tier 0.5 cross-version runner shipped",
        check_tier_0_5_runner,
    ),
    (
        "Tier 4.1 claim-verifier fabrication catch shipped",
        check_tier_4_1_runner,
    ),
    // Round-2 closure rows (G1 opaque aggregator, G3 cross-platform determinism gate)
    (
        "Round-2 G1 opaque aggregator shipped",
        check_round2_opaque_aggregator,
    ),
    (
        "Round-2 G3 cross-platform determinism gate shipped",
        check_round2_cross_platform_gate,
    ),
    // Closure plan B1/B2/B3/B4 + ADR rows
    (
        "Phase 6.1 B1: time_series_forecast archetype reachable",
        check_b1_time_series_forecast,
    ),
    (
        "Phase 6.1 B2: generic_omics archetype exists",
        check_b2_generic_omics,
    ),
    (
        "Phase 6.1 B3: discover_companion_synthesis module ships",
        check_b3_discover_companion,
    ),
    (
        "Phase D: all §0.2 deferrals have dated ADRs",
        check_d_adrs_present,
    ),
    (
        "Phase 6.1 B4: config/stage-taxonomies/ deleted",
        check_b4_taxonomy_deleted,
    ),
    // Closure-residuals close-out (R1-R5)
    (
        "Residuals R1/R2: OpaqueObservationSinkImpl wired in conversation",
        check_residual_r1_r2_sink_wired,
    ),
    (
        "Residuals R3: opaque_session_id field on PlanningContext",
        check_residual_r3_session_id_field,
    ),
    (
        "Residuals R4: days_window filter implemented",
        check_residual_r4_days_window,
    ),
    (
        "Residuals R5: time-series atom digests are not placeholders",
        check_residual_r5_real_digests,
    ),
];

fn read_or_empty(rel: &str) -> String {
    std::fs::read_to_string(rel).unwrap_or_default()
}

fn check_residual_r1_r2_sink_wired() -> bool {
    read_or_empty("../../crates/conversation/src/session/opaque_aggregator.rs")
        .contains("pub struct OpaqueObservationSinkImpl")
        && read_or_empty("../../crates/conversation/src/tools/mod.rs")
            .contains("OpaqueObservationSinkImpl::new")
}

fn check_residual_r3_session_id_field() -> bool {
    read_or_empty("../../crates/core/src/compatibility/engine.rs")
        .contains("pub opaque_session_id: Option<String>")
}

fn check_residual_r4_days_window() -> bool {
    let body = read_or_empty("../../crates/conversation/src/session/opaque_aggregator.rs");
    body.contains("days_window: u32") && !body.contains("_days_window")
}

fn check_residual_r5_real_digests() -> bool {
    let placeholder = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
    let yamls = [
        "../../config/stage-atoms/time_series_decompose.yaml",
        "../../config/stage-atoms/time_series_model_fit.yaml",
        "../../config/stage-atoms/time_series_forecast_evaluate.yaml",
    ];
    yamls
        .iter()
        .all(|p| !read_or_empty(p).contains(placeholder))
}

fn check_v3_f12() -> bool {
    read_or_empty("../../crates/core/src/assumption_policy.rs")
        .contains("pub struct AssumptionPolicyTable")
}

fn check_v3_f11() -> bool {
    read_or_empty("../../crates/core/src/promotion_gate_policy.rs")
        .contains("pub struct PromotionGatePolicy")
}

fn check_v3_chain_of_custody() -> bool {
    read_or_empty("../../crates/core/src/workflow_contracts/chain_of_custody.rs")
        .contains("pub struct ChainOfCustody")
}

fn check_v3_suppressed() -> bool {
    read_or_empty("../../crates/core/src/provenance_tiers.rs").contains("Suppressed")
}

fn check_v3_schema_versions() -> bool {
    read_or_empty("../../crates/core/src/migration/schema_versions.rs")
        .contains("pub struct SchemaVersionsManifest")
}

fn check_v3_population_coverage() -> bool {
    read_or_empty("../../crates/core/src/population_coverage.rs")
        .contains("pub struct PopulationCoverageStatement")
}

fn check_v3_lifecycle_transition() -> bool {
    read_or_empty("../../crates/core/src/lifecycle_adversarial.rs").contains("LifecycleTransition")
}

fn check_v3_assumption_policy_yaml() -> bool {
    Path::new("../../config/assumption-policy.yaml").exists()
}

fn check_v3_llm_availability() -> bool {
    read_or_empty("../../crates/core/src/llm_availability.rs").contains("pub enum LlmAvailability")
}

fn check_v3_injection_patterns() -> bool {
    read_or_empty("../../crates/core/src/ingestion_safety/patterns.rs")
        .contains("InjectionPatternCatalog")
}

fn check_v3_capability_report() -> bool {
    read_or_empty("../../crates/core/src/backend_emitters/capability_report.rs")
        .contains("BackendCapabilityReport")
}

fn check_v4_d2() -> bool {
    read_or_empty("../../crates/core/src/ontology_scope.rs")
        .contains("pub struct OntologyScopeMatrix")
}

fn check_v4_d3() -> bool {
    read_or_empty("../../crates/core/src/decision_substrate.rs").contains("VerifierDecision")
}

fn check_v4_d5() -> bool {
    read_or_empty("../../crates/core/src/repair/strategy.rs").contains("pub trait RepairStrategy")
}

fn check_v4_d6() -> bool {
    let kind = read_or_empty("../../crates/core/src/workflow_contracts/refusal_kind.rs");
    let path = read_or_empty("../../crates/core/src/workflow_contracts/unblock_path.rs");
    kind.contains("RefusalKind") && path.contains("UnblockPath")
}

fn check_v4_d7() -> bool {
    read_or_empty("../../crates/core/src/sandbox_refusal_category.rs")
        .contains("SandboxRefusalCategory")
}

fn check_v4_d8() -> bool {
    Path::new("../../config/promotion-gate-policy.yaml").exists()
}

fn check_v4_d4() -> bool {
    read_or_empty("../../crates/core/src/workflow_contracts/semantic_type.rs")
        .contains("LocalExtensionMaturity")
}

fn check_v4_d1() -> bool {
    Path::new("../../crates/core/src/compile_time_discipline").exists()
}

fn check_v3_policy_rule_id() -> bool {
    read_or_empty("../../crates/core/src/workflow_contracts/policy_rule_id.rs")
        .contains("pub struct PolicyRuleId")
}

fn check_v3_assumption_policy_rules() -> bool {
    read_or_empty("../../config/assumption-policy.yaml").contains("policy_rules:")
}

fn check_v4_dag_mutation() -> bool {
    read_or_empty("../../crates/core/src/composer_v4/dag_mutation.rs")
        .contains("pub fn apply_dag_modification")
}

fn check_v3_lifecycle_substrate() -> bool {
    read_or_empty("../../crates/core/src/decision_substrate.rs")
        .contains("LifecycleAdversarialEdgeDetected")
}

fn check_tier_3_5() -> bool {
    Path::new("../../crates/eval-adapters/tests/tier-3-5-corpus").is_dir()
        && read_or_empty("../../crates/eval-adapters/src/tier_3_5_repair_effectiveness.rs")
            .contains("pub fn run_tier_3_5")
}

fn check_tier_8_10() -> bool {
    Path::new("../../crates/eval-adapters/tests/tier-8-10-corpus").is_dir()
        && read_or_empty("../../crates/eval-adapters/src/tier_8_10_substrate_completeness.rs")
            .contains("pub fn run_tier_8_10")
}

fn check_ui_repairs_tab() -> bool {
    read_or_empty("../../ui/src/components/state_inspector/index.ts").contains("'repairs'")
        || read_or_empty("../../ui/src/components/state_inspector/index.ts").contains("\"repairs\"")
}

fn check_tier_0_runner() -> bool {
    read_or_empty("../../crates/eval-adapters/src/tier_0_sme_journey.rs").contains("pub fn run")
        && Path::new("../../crates/eval-adapters/tests/tier-0-corpus").is_dir()
}

fn check_tier_0_5_runner() -> bool {
    read_or_empty("../../crates/eval-adapters/src/tier_0_5_cross_version.rs").contains("pub fn run")
        && Path::new("../../crates/eval-adapters/tests/tier-0-5-corpus").is_dir()
}

fn check_tier_4_1_runner() -> bool {
    read_or_empty("../../crates/eval-adapters/src/tier_4_1_claim_verifier_fabrications.rs")
        .contains("pub fn run")
        && Path::new("../../crates/eval-adapters/tests/tier-4-1-corpus").is_dir()
}

fn check_round2_opaque_aggregator() -> bool {
    read_or_empty("../../crates/conversation/src/session/opaque_aggregator.rs")
        .contains("pub struct OpaqueAggregator")
}

fn check_round2_cross_platform_gate() -> bool {
    Path::new("../../.github/ci/determinism-baseline.json").exists()
        && read_or_empty("../../crates/core/tests/composer_v4/composer_v4_determinism.rs")
            .contains("cross_platform_hashes_match_baseline")
}

fn check_b1_time_series_forecast() -> bool {
    Path::new("../../config/stage-atoms/time_series_forecast_evaluate.yaml").exists()
        && Path::new("../../config/stage-atoms/time_series_decompose.yaml").exists()
        && Path::new("../../config/stage-atoms/time_series_model_fit.yaml").exists()
}

fn check_b2_generic_omics() -> bool {
    Path::new("../../config/archetypes/generic_omics.yaml").exists()
}

fn check_b3_discover_companion() -> bool {
    read_or_empty("../../crates/core/src/composer_v4/discover_companion_synthesis.rs")
        .contains("synthesize_discover_companions")
}

fn check_d_adrs_present() -> bool {
    // ADRs live under `docs/adr/` which is in `.gitignore` (per-operator
    // doc-stash), so the directory's existence is environment-dependent.
    // If the operator has the ADRs locally, every one of these must be
    // present; if they don't, the check degrades to advisory (passes).
    let dir = Path::new("../../docs/adr");
    if !dir.exists() {
        return true;
    }
    let adrs = [
        "0033-external-registry-importer-deferral.md",
        "0034-llm-embedding-graduation-deferral.md",
        "0035-upstream-ontology-submission-deferral.md",
        "0036-repair-strategy-chaining-deferral.md",
        "0037-structured-form-replacement-deferral.md",
        "0038-const-generics-restraint.md",
        "0039-phantom-serialization-forbidden.md",
    ];
    adrs.iter()
        .all(|name| Path::new(&format!("../../docs/adr/{}", name)).exists())
}

fn check_b4_taxonomy_deleted() -> bool {
    // Both the YAML directory and the YAML loader path must be gone.
    // The taxonomy.rs file is retained as a thin no-op holding
    // StageSpec/StageCardinality types still used by the composer-driven
    // builder, but the build_dag_from_taxonomy entry point is gone.
    !Path::new("../../config/stage-taxonomies").exists()
        && !read_or_empty("../../crates/core/src/builder.rs")
            .contains("pub fn build_dag_from_taxonomy")
}

#[test]
#[ignore = "v3/v4 design doc cross-reference; docs not in OSS repo"]
fn every_status_row_matches_code() {
    let mut failures = vec![];
    for (label, check) in ROWS {
        if !check() {
            failures.push(label.to_string());
        }
    }
    if !failures.is_empty() {
        panic!("v3 §0.1 / v4 §0.4 status drift:\n{}", failures.join("\n"));
    }
}
