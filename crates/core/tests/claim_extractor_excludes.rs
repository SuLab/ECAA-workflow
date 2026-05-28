//! Regression test: common-noun acronyms are excluded from extracted claims.
//!
//! The base entity pattern `[A-Z][A-Z0-9]{1,}` matches all-caps tokens
//! broadly, which is correct for gene symbols (TP53, BRCA1, MYC) but
//! also spuriously matches common-noun acronyms (RNA, PCR, DNA, WHO).
//! `entityNameExcludePatterns` provides an anchored-regex denylist that
//! filters those tokens out after the entity scan, so the verifier does
//! not try to look up "WHO" in a result table.

use serde_json::json;

fn policy_with_excludes() -> serde_json::Value {
    json!({
        "verifiableEntities": {
            "enabled": true,
            "entityNamePatterns": ["[A-Z][A-Z0-9]{1,}"],
            "entityNameExcludePatterns": [
                "^USA$", "^WHO$", "^CDC$", "^BBC$", "^EU$", "^US$", "^UK$",
                "^PCR$", "^QPCR$", "^DNA$", "^RNA$", "^ATP$", "^ADP$", "^AMP$",
                "^NGS$", "^ELISA$", "^SDS$", "^PAGE$", "^BSA$", "^HRP$",
                "^FDA$", "^EMA$", "^ISO$", "^GMP$",
                "^MS$", "^MS2$", "^MS3$",
                "^IVD$", "^IVT$"
            ],
            "directionVocab": {
                "up": ["upregulated", "increased", "elevated"],
                "down": ["downregulated", "decreased", "reduced"]
            },
            "effectSizeColumns": ["log2FC", "logFC"],
            "entityColumns": ["gene", "symbol"],
            "pvalueColumns": ["pvalue", "padj"]
        }
    })
}

fn policy_without_excludes() -> serde_json::Value {
    json!({
        "verifiableEntities": {
            "enabled": true,
            "entityNamePatterns": ["[A-Z][A-Z0-9]{1,}"],
            "directionVocab": {
                "up": ["upregulated", "increased", "elevated"],
                "down": ["downregulated", "decreased", "reduced"]
            },
            "effectSizeColumns": ["log2FC", "logFC"],
            "entityColumns": ["gene", "symbol"],
            "pvalueColumns": ["pvalue", "padj"]
        }
    })
}

/// Common-noun acronyms must be excluded; true gene symbols must survive.
#[test]
fn denylist_excludes_common_noun_acronyms_but_keeps_gene_symbols() {
    use ecaa_workflow_core::claim_extractor::{extract_claims, ExtractorConfig};

    let cfg = ExtractorConfig::from_policy(&policy_with_excludes()).unwrap();

    let text = concat!(
        "RNA-seq analysis of TP53 in BRCA1+ patients showed elevated PCR amplification ",
        "and DNA quality. The WHO recommends monitoring IL6 expression levels."
    );
    let claims = extract_claims(text, &cfg);

    let entities: Vec<&str> = claims.iter().map(|c| c.entity.as_str()).collect();

    // Gene symbols that must be present.
    assert!(
        entities.contains(&"TP53"),
        "TP53 must be extracted; got {:?}",
        entities
    );
    assert!(
        entities.contains(&"BRCA1"),
        "BRCA1 must be extracted; got {:?}",
        entities
    );
    assert!(
        entities.contains(&"IL6"),
        "IL6 must be extracted; got {:?}",
        entities
    );

    // Common-noun acronyms that must be absent.
    assert!(
        !entities.contains(&"RNA"),
        "RNA must be excluded; got {:?}",
        entities
    );
    assert!(
        !entities.contains(&"PCR"),
        "PCR must be excluded; got {:?}",
        entities
    );
    assert!(
        !entities.contains(&"DNA"),
        "DNA must be excluded; got {:?}",
        entities
    );
    assert!(
        !entities.contains(&"WHO"),
        "WHO must be excluded; got {:?}",
        entities
    );
}

/// Additional single-letter + all-caps gene symbols must survive (MYC, JUN, FOS, RAS).
#[test]
fn gene_symbols_without_digits_survive_denylist() {
    use ecaa_workflow_core::claim_extractor::{extract_claims, ExtractorConfig};

    let cfg = ExtractorConfig::from_policy(&policy_with_excludes()).unwrap();

    let text = "MYC, JUN, FOS, and RAS were all upregulated in the treated group (padj=0.001).";
    let claims = extract_claims(text, &cfg);
    let entities: Vec<&str> = claims.iter().map(|c| c.entity.as_str()).collect();

    for sym in &["MYC", "JUN", "FOS", "RAS"] {
        assert!(
            entities.contains(sym),
            "{sym} must survive the denylist; got {:?}",
            entities
        );
    }
}

/// Without `entityNameExcludePatterns` the acronyms ARE extracted (baseline
/// confirms the test is non-trivial — the denylist is what filters them).
#[test]
fn without_denylist_common_nouns_are_present() {
    use ecaa_workflow_core::claim_extractor::{extract_claims, ExtractorConfig};

    let cfg = ExtractorConfig::from_policy(&policy_without_excludes()).unwrap();

    let text = "RNA-seq analysis of TP53 showed elevated PCR amplification and DNA quality. The WHO recommends it.";
    let claims = extract_claims(text, &cfg);
    let entities: Vec<&str> = claims.iter().map(|c| c.entity.as_str()).collect();

    // Without the denylist, spurious matches ARE present — this is the
    // baseline that proves the denylist is load-bearing.
    let has_spurious = entities.contains(&"RNA")
        || entities.contains(&"PCR")
        || entities.contains(&"DNA")
        || entities.contains(&"WHO");
    assert!(
        has_spurious,
        "without denylist, at least one common-noun acronym must appear; got {:?}",
        entities
    );
}

/// The denylist is backward-compatible: a policy that omits
/// `entityNameExcludePatterns` loads successfully with an empty denylist.
#[test]
fn missing_field_loads_with_empty_denylist() {
    use ecaa_workflow_core::claim_extractor::ExtractorConfig;

    let result = ExtractorConfig::from_policy(&policy_without_excludes());
    assert!(
        result.is_ok(),
        "policy without entityNameExcludePatterns must load OK: {:?}",
        result.err()
    );
}

/// MS/MS2/MS3 (mass-spec abbreviations) must be excludable via the denylist.
#[test]
fn mass_spec_abbreviations_excluded() {
    use ecaa_workflow_core::claim_extractor::{extract_claims, ExtractorConfig};

    let cfg = ExtractorConfig::from_policy(&policy_with_excludes()).unwrap();

    // MS2 and MS3 are in the default denylist; ACTB is a real gene.
    let text = "MS2 fragmentation of ACTB peptides was analyzed using MS3 on an Orbitrap.";
    let claims = extract_claims(text, &cfg);
    let entities: Vec<&str> = claims.iter().map(|c| c.entity.as_str()).collect();

    assert!(
        entities.contains(&"ACTB"),
        "ACTB must be extracted; got {:?}",
        entities
    );
    assert!(
        !entities.contains(&"MS2"),
        "MS2 must be excluded; got {:?}",
        entities
    );
    assert!(
        !entities.contains(&"MS3"),
        "MS3 must be excluded; got {:?}",
        entities
    );
}
