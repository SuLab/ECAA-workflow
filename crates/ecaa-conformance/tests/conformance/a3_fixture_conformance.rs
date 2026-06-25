//! A3 — every committed `testdata/**/ro-crate-metadata.json` fixture must
//! match the ECAA v0.2 descriptor contract:
//!
//!   (a) its `ro-crate-metadata.json` descriptor entry declares the required
//!       `conformsTo` profile IRIs its graph TRUTHFULLY satisfies, under the
//!       EXECUTION-AWARE Invariant-6 rule (user-approved ecaa/v0.2 amendment,
//!       2026-06-25):
//!         - a PLAN crate (no real run `CreateAction` with an `instrument`)
//!           must declare ⊇ the 3 DEFINITION profiles
//!           (`ecaa_workflow_types::consts::PLAN_PROFILE_IRIS`);
//!         - an EXECUTED crate (≥1 real run `CreateAction` with an
//!           `instrument`) must declare all 6
//!           (`ecaa_workflow_types::consts::REQUIRED_PROFILE_IRIS`).
//!       The plan-vs-executed split mirrors the Rust emitter's
//!       `graph_has_run_create_action` and the SHACL `SubstrateValidityShape`.
//!       Both sets are read from the canonical consts, not hardcoded.
//!   (b) it contains NO malformed EDAM IRI in the `<word>:<digits>` colon form
//!       (`https://edamontology.org/operation:3222`) — EDAM IRIs are normative
//!       only in the underscore form (`.../operation_3222`).
//!
//! REGEN DEPENDENCY (read before treating a failure as a regression):
//! the committed plan fixtures under `testdata/emitted-packages/` may still
//! OVER-declare the 3 WRROC run profiles (a pre-amendment emit wrote all 6 on a
//! plan crate). The execution-aware rule is a ⊇ (superset) floor, so an
//! over-declaring plan crate still satisfies the 3-profile requirement and this
//! gate stays green; the run-profile cleanup on plan fixtures is the fixture
//! refresh task's job. This test asserts the CORRECT END STATE and permanently
//! guards against UNDER-declaration drift (a plan crate missing a definition
//! profile, or an executed crate missing any of the 6).

use std::path::{Path, PathBuf};

/// Repo root = two ancestors up from `crates/ecaa-conformance`.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .to_path_buf()
}

/// Recursively collect every `ro-crate-metadata.json` under `testdata/`.
fn collect_fixtures(root: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) => panic!("read_dir({}) failed: {e}", root.display()),
    };
    for entry in entries {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        let ft = entry.file_type().expect("file type");
        if ft.is_dir() {
            collect_fixtures(&path, out);
        } else if path.file_name().and_then(|n| n.to_str()) == Some("ro-crate-metadata.json") {
            out.push(path);
        }
    }
}

/// Parse the `ro-crate-metadata.json` descriptor entry's `conformsTo` IRIs.
fn conforms_to_iris(metadata: &serde_json::Value) -> Vec<String> {
    let graph = metadata["@graph"]
        .as_array()
        .expect("@graph must be an array");
    let descriptor = graph
        .iter()
        .find(|e| e["@id"] == "ro-crate-metadata.json")
        .expect("descriptor entry (@id == ro-crate-metadata.json) must exist");
    match descriptor["conformsTo"].as_array() {
        Some(arr) => arr
            .iter()
            .filter_map(|c| c["@id"].as_str().map(str::to_string))
            .collect(),
        None => Vec::new(),
    }
}

/// Does the RO-Crate `@graph` carry ≥1 REAL executed run `CreateAction`?
///
/// Mirrors the emitter's `graph_has_run_create_action`
/// (`crates/core/src/ro_crate.rs`) and the `_project.py`
/// `graph_records_execution` helper: a "real run action" is an entity whose
/// `@type` contains `CreateAction` AND that carries an `instrument` with an
/// `@id`. Decides which profile floor the execution-aware Invariant-6 rule
/// holds the crate to (plan → 3 definition profiles, executed → all 6).
fn records_execution(metadata: &serde_json::Value) -> bool {
    let Some(graph) = metadata["@graph"].as_array() else {
        return false;
    };
    graph.iter().any(|e| {
        let is_create_action = match &e["@type"] {
            serde_json::Value::String(s) => s == "CreateAction",
            serde_json::Value::Array(a) => a.iter().any(|t| t.as_str() == Some("CreateAction")),
            _ => false,
        };
        is_create_action && e["instrument"]["@id"].as_str().is_some()
    })
}

/// Scan the raw JSON text for the malformed EDAM colon-form IRI. We scan the
/// raw bytes (not parsed values) so a colon-form IRI anywhere in the document
/// — `@id`, free-text, nested object — is caught. Returns the offending
/// substrings (deduped, sorted) for a precise failure message.
fn malformed_edam_iris(raw: &str) -> Vec<String> {
    const PREFIX: &str = "https://edamontology.org/";
    let mut hits: Vec<String> = Vec::new();
    let bytes = raw.as_bytes();
    let mut search_from = 0;
    while let Some(rel) = raw[search_from..].find(PREFIX) {
        let start = search_from + rel;
        let mut i = start + PREFIX.len();
        // Consume the term label (ASCII letters).
        let label_start = i;
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        let has_label = i > label_start;
        // A malformed IRI is `<word>:<digit...>`. The normative form uses `_`.
        if has_label && i < bytes.len() && bytes[i] == b':' {
            let digits_start = i + 1;
            let mut j = digits_start;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > digits_start {
                hits.push(raw[start..j].to_string());
            }
        }
        search_from = start + PREFIX.len();
    }
    hits.sort();
    hits.dedup();
    hits
}

#[test]
fn every_testdata_fixture_declares_execution_aware_iris_and_underscore_edam() {
    let testdata = repo_root().join("testdata");
    assert!(
        testdata.is_dir(),
        "testdata/ must exist at {}",
        testdata.display()
    );

    let mut fixtures: Vec<PathBuf> = Vec::new();
    collect_fixtures(&testdata, &mut fixtures);
    fixtures.sort();
    assert!(
        !fixtures.is_empty(),
        "expected at least one testdata/**/ro-crate-metadata.json fixture"
    );

    // Execution-aware Invariant-6 floors (canonical consts, not hardcoded):
    // an executed crate must declare all 6; a plan crate ⊇ the 3 definition
    // profiles.
    let executed_required: &[&str] = ecaa_workflow_types::consts::REQUIRED_PROFILE_IRIS;
    let plan_required: &[&str] = ecaa_workflow_types::consts::PLAN_PROFILE_IRIS;
    let mut iri_failures: Vec<String> = Vec::new();
    let mut edam_failures: Vec<String> = Vec::new();

    for fixture in &fixtures {
        let rel = fixture
            .strip_prefix(repo_root())
            .unwrap_or(fixture)
            .display()
            .to_string();
        let raw = std::fs::read_to_string(fixture)
            .unwrap_or_else(|e| panic!("reading {}: {e}", fixture.display()));
        let metadata: serde_json::Value = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("parsing {}: {e}", fixture.display()));

        // (a) the required conformsTo IRIs for THIS crate's execution mode are
        // present (plan → 3 definition profiles; executed → all 6). The rule is
        // a ⊇ floor: a crate MAY declare more, so an over-declaring plan crate
        // still passes the plan floor — only UNDER-declaration is a violation.
        let executed = records_execution(&metadata);
        let (mode, required) = if executed {
            ("executed", executed_required)
        } else {
            ("plan", plan_required)
        };
        let declared = conforms_to_iris(&metadata);
        let missing: Vec<&str> = required
            .iter()
            .copied()
            .filter(|iri| !declared.iter().any(|d| d.as_str() == *iri))
            .collect();
        if !missing.is_empty() {
            iri_failures.push(format!(
                "{rel}: [{mode}] missing {} required conformsTo IRI(s) {missing:?} (declared {declared:?})",
                missing.len()
            ));
        }

        // (b) no malformed colon-form EDAM IRI.
        let bad = malformed_edam_iris(&raw);
        if !bad.is_empty() {
            edam_failures.push(format!("{rel}: malformed colon-form EDAM IRI(s) {bad:?}"));
        }
    }

    assert!(
        iri_failures.is_empty() && edam_failures.is_empty(),
        "A3 fixture-conformance failed over {} fixture(s).\n\
         This gate asserts the EXECUTION-AWARE Invariant-6 floor (plan crate ⊇ \
         the 3 definition profiles; executed crate = all 6) plus underscore-form \
         EDAM IRIs. A failure means a crate UNDER-declares its required profiles \
         (or carries a colon-form EDAM IRI). Do NOT hand-edit fixtures to silence \
         this; regenerate them with the built emitter.\n\
         --- conformsTo failures ({}) ---\n{}\n\
         --- EDAM colon-form failures ({}) ---\n{}",
        fixtures.len(),
        iri_failures.len(),
        iri_failures.join("\n"),
        edam_failures.len(),
        edam_failures.join("\n"),
    );
}

#[cfg(test)]
mod self_tests {
    use super::{malformed_edam_iris, records_execution};

    #[test]
    fn records_execution_discriminates_plan_from_executed() {
        // Plan crate: a HowToStep definition but NO CreateAction → plan.
        let plan = serde_json::json!({
            "@graph": [
                {"@id": "#step-de", "@type": "HowToStep"}
            ]
        });
        assert!(
            !records_execution(&plan),
            "a crate with no CreateAction must be classified as a PLAN crate"
        );

        // Executed crate: a real run CreateAction with an instrument @id.
        let executed = serde_json::json!({
            "@graph": [
                {
                    "@id": "#action-de",
                    "@type": ["CreateAction", "prov:Activity"],
                    "instrument": {"@id": "#step-de"},
                    "result": {"@id": "outputs/de.csv"}
                }
            ]
        });
        assert!(
            records_execution(&executed),
            "a crate with a real CreateAction carrying an instrument must be EXECUTED"
        );

        // A CreateAction WITHOUT an instrument is not a real run action.
        let no_instrument = serde_json::json!({
            "@graph": [
                {"@id": "#a", "@type": "CreateAction", "result": {"@id": "x"}}
            ]
        });
        assert!(
            !records_execution(&no_instrument),
            "a CreateAction lacking an instrument @id must NOT count as execution"
        );

        // Compile-time SME resolution entities are plain Action, not counted.
        let plain_action = serde_json::json!({
            "@graph": [
                {"@id": "#a", "@type": "Action", "instrument": {"@id": "#step-de"}}
            ]
        });
        assert!(
            !records_execution(&plain_action),
            "a plain Action (not CreateAction) must NOT count as execution"
        );
    }

    #[test]
    fn detects_colon_form_and_ignores_underscore_form() {
        let colon = r#"{"@id":"https://edamontology.org/operation:3222"}"#;
        assert_eq!(
            malformed_edam_iris(colon),
            vec!["https://edamontology.org/operation:3222".to_string()],
            "colon-form EDAM IRI must be flagged"
        );

        let underscore = r#"{"@id":"https://edamontology.org/operation_3222"}"#;
        assert!(
            malformed_edam_iris(underscore).is_empty(),
            "underscore-form EDAM IRI must NOT be flagged"
        );

        // Non-EDAM colon IRIs (e.g. w3id profile IRIs) must not be flagged.
        let other = r#"{"@id":"https://w3id.org/ro/wfrun/process/0.5"}"#;
        assert!(
            malformed_edam_iris(other).is_empty(),
            "non-EDAM IRIs must NOT be flagged"
        );
    }

    #[test]
    fn dedups_repeated_colon_iris() {
        let raw = r#"a https://edamontology.org/topic:3169 b https://edamontology.org/topic:3169"#;
        assert_eq!(
            malformed_edam_iris(raw),
            vec!["https://edamontology.org/topic:3169".to_string()]
        );
    }
}
