//! Schema + classifier sanity gate for the auditability corpus.
//!
//! The corpus at `tests/auditability-corpus/` is the 13×30 = 390-claim
//! pre-registration artifact for PAR-26-040 grant §Aim 3B. Each study
//! directory carries a `claims.yaml` file whose schema is documented in
//! `tests/auditability-corpus/README.md`. This test loads every file,
//! validates the schema, and for each filled (non-stub) claim checks
//! that the auto-classifier in `claim_extractor::classify_contract`
//! agrees with the declared `contract` field. The auto-classifier is
//! the same heuristic the runtime verifier uses to route narrative
//! claims to the appropriate contract class, so disagreement here
//! means an HCI lead has either (a) declared a non-matching contract
//! by mistake or (b) hit a real edge case the classifier needs to
//! learn.
//!
//! Pre-registration discipline forbids retroactive claim mutation:
//! when this test flags a row, fix the classifier or fix the YAML; do
//! not silently swap the contract field after an eval run.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use ecaa_workflow_core::claim_contract::ClaimContract;
use ecaa_workflow_core::claim_extractor::classify_contract;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ClaimsFile {
    schema_version: String,
    study_id: String,
    claims: Vec<ClaimEntry>,
}

#[derive(Debug, Deserialize)]
struct ClaimEntry {
    id: String,
    text: String,
    contract: ClaimContract,
    source_table: String,
    // The expected_value column is intentionally untyped — the verifier
    // accepts numeric, string, or null. We only need to know whether
    // the slot was filled, not what value lives there. `tolerance` is
    // unused here and dropped from the struct; serde silently ignores
    // unknown fields, so the YAML schema stays intact for the verifier.
    #[serde(default)]
    expected_value: serde_yml::Value,
}

impl ClaimEntry {
    /// A claim is considered "filled" when either (a) the text does not
    /// start with the canonical PLACEHOLDER stub marker or (b) the
    /// expected_value column carries a non-null value. Both checks are
    /// needed: a stub keeps the PLACEHOLDER text AND a null value, while
    /// a fill replaces at least the text. Auto-extracted claims always
    /// replace both.
    fn is_filled(&self) -> bool {
        let stub_text = self.text.starts_with("PLACEHOLDER");
        let null_value = self.expected_value.is_null();
        !(stub_text && null_value)
    }
}

fn corpus_root() -> PathBuf {
    // Locate the corpus relative to the workspace root. Cargo runs
    // tests with CWD at the crate directory (`crates/core`), so walk
    // up to the workspace root then into `tests/auditability-corpus`.
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .ancestors()
        .find(|p| p.join("tests/auditability-corpus").is_dir())
        .map(|p| p.join("tests/auditability-corpus"))
        .expect("auditability-corpus dir must exist at workspace root")
}

fn load_corpus() -> Vec<(PathBuf, ClaimsFile)> {
    let root = corpus_root();
    let mut out: Vec<(PathBuf, ClaimsFile)> = Vec::new();
    let entries =
        fs::read_dir(&root).unwrap_or_else(|e| panic!("read_dir {}: {e}", root.display()));
    for entry in entries {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let claims_path = path.join("claims.yaml");
        if !claims_path.exists() {
            continue;
        }
        let raw = fs::read_to_string(&claims_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", claims_path.display()));
        let parsed: ClaimsFile = serde_yml::from_str(&raw)
            .unwrap_or_else(|e| panic!("parse {}: {e}", claims_path.display()));
        out.push((claims_path, parsed));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Every study YAML loads, declares the documented schema version, and
/// carries exactly 30 claims per the pre-registration baseline. Drift
/// down on count is a SemVer-class break of the eval contract.
#[test]
fn every_claims_file_parses_and_has_thirty_entries() {
    let corpus = load_corpus();
    assert!(
        !corpus.is_empty(),
        "auditability corpus must contain at least one study"
    );
    let mut seen_ids: HashSet<String> = HashSet::new();
    for (path, file) in &corpus {
        assert_eq!(
            file.schema_version,
            "0.1",
            "{} schema_version drift",
            path.display(),
        );
        assert_eq!(
            file.claims.len(),
            30,
            "{} should hold 30 claims, found {}",
            path.display(),
            file.claims.len(),
        );
        assert!(
            !file.study_id.is_empty(),
            "{} has empty study_id",
            path.display(),
        );
        // Per-claim required fields and id uniqueness across the corpus.
        for claim in &file.claims {
            assert!(
                !claim.id.is_empty(),
                "{} has a claim with empty id",
                path.display()
            );
            assert!(
                !claim.text.is_empty(),
                "{} claim {} text is empty",
                path.display(),
                claim.id
            );
            assert!(
                !claim.source_table.is_empty(),
                "{} claim {} source_table is empty",
                path.display(),
                claim.id
            );
            assert!(
                claim.id.starts_with(&file.study_id),
                "{} claim id {} does not start with study_id {}",
                path.display(),
                claim.id,
                file.study_id,
            );
            assert!(
                seen_ids.insert(claim.id.clone()),
                "duplicate claim id across corpus: {}",
                claim.id,
            );
        }
    }
}

/// At least MIN_FILLED_PER_STUDY claims per study must be non-stub.
/// The target is 40% fill (12 of 30); the script
/// `scripts/auditability_claim_extractor.py` enforces this floor at
/// generation time. If a fresh run drops a study below the floor it
/// likely means a plan's "## 5. Acceptance criteria" section was
/// rewritten in a way the extractor no longer mines — fix the
/// extractor, not the threshold.
#[test]
fn corpus_meets_forty_percent_fill_target() {
    const MIN_FILLED_PER_STUDY: usize = 12;
    let corpus = load_corpus();
    for (path, file) in &corpus {
        let filled = file.claims.iter().filter(|c| c.is_filled()).count();
        assert!(
            filled >= MIN_FILLED_PER_STUDY,
            "{}: only {} filled claims, need ≥{}",
            path.display(),
            filled,
            MIN_FILLED_PER_STUDY,
        );
    }
}

/// `classify_contract(claim.text)` should agree with the declared
/// contract field on every filled claim. We allow `NumericTableLookup`
/// as a permissive fallback: when the auto-classifier defaults to that
/// catch-all class but the YAML declares a more specific contract (e.g.
/// `Categorical` for a stage-existence row), the YAML wins — the
/// declared class is strictly more specific. The opposite direction is
/// the failure case: the classifier identified a specific contract
/// the HCI lead missed.
#[test]
fn auto_classifier_agrees_with_declared_contract() {
    let corpus = load_corpus();
    let mut mismatches: Vec<String> = Vec::new();
    let mut filled_total = 0usize;
    for (path, file) in &corpus {
        for claim in &file.claims {
            if !claim.is_filled() {
                continue;
            }
            filled_total += 1;
            let auto = classify_contract(&claim.text);
            if auto == claim.contract {
                continue;
            }
            // Asymmetric tolerance: allow the YAML to be more specific
            // than the auto-classifier's default. The classifier's
            // fallback is `NumericTableLookup`, so when the auto answer
            // is the fallback we trust the YAML's explicit class.
            if auto == ClaimContract::NumericTableLookup {
                continue;
            }
            mismatches.push(format!(
                "{} {}: text={:?} declared={:?} auto={:?}",
                path.display(),
                claim.id,
                claim.text,
                claim.contract,
                auto,
            ));
        }
    }
    assert!(
        filled_total > 0,
        "no filled claims found across corpus — extractor never ran?"
    );
    assert!(
        mismatches.is_empty(),
        "{} contract mismatches:\n{}",
        mismatches.len(),
        mismatches.join("\n"),
    );
}
