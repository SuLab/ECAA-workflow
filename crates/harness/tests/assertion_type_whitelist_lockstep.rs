//! Lockstep pin between the harness's `run_assertion` match and the SME-bound
//! merge whitelist `ecaa_workflow_core::validation_bound::SUPPORTED_ASSERTION_TYPES`.
//!
//! Two lists must agree, and nothing in the type system makes them agree:
//!
//! - `run_assertion` (`crates/harness/src/main.rs`) is the only evaluator of a
//!   validation-contract assertion. Its `match atype` arms are the set of
//!   assertion types that can actually pass.
//! - `SUPPORTED_ASSERTION_TYPES` is the whitelist `merge_into_contract` filters
//!   SME-authored bounds through, and `SmeEdits::validate_validation_bound`
//!   rejects on.
//!
//! Drift in either direction is a silent defect with no compile error:
//!
//! - Whitelisted but unimplemented → `run_assertion` returns `false` forever, so
//!   a `required` bound permanently re-blocks its stage regardless of the result.
//! - Implemented but not whitelisted → an archetype contract may use the type
//!   while an SME-authored bound naming the same type is dropped at merge with
//!   no diagnostic, so the SME's constraint silently never runs.
//!
//! The second direction is what actually drifted: `run_assertion` grew
//! `table_header_has_all_groups` and `cross_stage_table_handoff` (both used by
//! shipped archetype contracts under `config/downstream-policy/`, both present in
//! those contracts' `.schema.json` `assertion_type` enums) while the whitelist
//! stayed at fourteen entries.
//!
//! `run_assertion` lives in the harness BIN target, so no test can call it. This
//! test therefore extracts its arms from source. The extraction is anchored on
//! `fn run_assertion` → `match atype {` → the trailing `_ =>` arm, and asserts it
//! found that terminator, so a reformat that defeats the parse fails loudly
//! instead of vacuously passing.

use std::collections::BTreeSet;

/// Arm-pattern lines sit at exactly this indentation inside `run_assertion`'s
/// `match atype` block. Deeper string literals (e.g. the inner `match stat`
/// arms) and arm bodies are excluded by the exact-prefix test.
const ARM_INDENT: &str = "        ";

/// Every string literal appearing in an arm PATTERN of `run_assertion`'s
/// `match atype` block, in source order.
fn run_assertion_arm_types(src: &str) -> Vec<String> {
    let fn_start = src
        .find("fn run_assertion(")
        .expect("crates/harness/src/main.rs must define `fn run_assertion(`");
    let match_start = src[fn_start..]
        .find("match atype {")
        .map(|off| fn_start + off)
        .expect("`run_assertion` must dispatch on `match atype {`");

    let mut out: Vec<String> = Vec::new();
    let mut saw_wildcard = false;
    for line in src[match_start..].lines().skip(1) {
        // The wildcard arm at arm indentation closes the dispatch.
        if line.starts_with(ARM_INDENT) && line[ARM_INDENT.len()..].starts_with("_ =>") {
            saw_wildcard = true;
            break;
        }
        // An arm pattern starts at exactly ARM_INDENT with a string literal.
        // `line[ARM_INDENT.len()..]` must not itself begin with a space, or a
        // nested literal would be mistaken for an arm.
        if !line.starts_with(ARM_INDENT) {
            continue;
        }
        let rest = &line[ARM_INDENT.len()..];
        if !rest.starts_with('"') {
            continue;
        }
        // Only the pattern side of `=>` carries type literals.
        let pattern = rest.split("=>").next().unwrap_or(rest);
        for piece in pattern.split('|') {
            let piece = piece.trim();
            if let Some(inner) = piece
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .filter(|s| !s.is_empty())
            {
                out.push(inner.to_string());
            }
        }
    }
    assert!(
        saw_wildcard,
        "did not reach `run_assertion`'s `_ =>` arm at the expected indentation — \
         the extraction below is unreliable; fix this test alongside the reformat \
         (extracted so far: {out:?})"
    );
    out
}

fn read_harness_main() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {} : {e}", path.display()))
}

#[test]
fn run_assertion_arms_and_sme_whitelist_are_in_lockstep() {
    use ecaa_workflow_core::validation_bound::SUPPORTED_ASSERTION_TYPES;

    let implemented: Vec<String> = run_assertion_arm_types(&read_harness_main());
    let implemented_set: BTreeSet<&str> = implemented.iter().map(String::as_str).collect();
    let whitelisted: BTreeSet<&str> = SUPPORTED_ASSERTION_TYPES.iter().copied().collect();

    // A duplicated literal would make the counts disagree with the sets and
    // hide a real omission behind an accidental match.
    assert_eq!(
        implemented.len(),
        implemented_set.len(),
        "duplicate assertion-type literal in `run_assertion`: {implemented:?}"
    );
    assert_eq!(
        SUPPORTED_ASSERTION_TYPES.len(),
        whitelisted.len(),
        "duplicate entry in SUPPORTED_ASSERTION_TYPES: {SUPPORTED_ASSERTION_TYPES:?}"
    );

    let whitelisted_but_unimplemented: Vec<&&str> =
        whitelisted.difference(&implemented_set).collect();
    assert!(
        whitelisted_but_unimplemented.is_empty(),
        "SUPPORTED_ASSERTION_TYPES entries the harness `run_assertion` does not \
         implement: {whitelisted_but_unimplemented:?} — an SME bound naming one \
         resolves to `false` forever and permanently re-blocks its stage. Either \
         implement the arm in crates/harness/src/main.rs::run_assertion or drop \
         the entry."
    );

    let implemented_but_not_whitelisted: Vec<&&str> =
        implemented_set.difference(&whitelisted).collect();
    assert!(
        implemented_but_not_whitelisted.is_empty(),
        "assertion types `run_assertion` implements but SUPPORTED_ASSERTION_TYPES \
         omits: {implemented_but_not_whitelisted:?} — an archetype contract may use \
         them while an SME-authored bound naming the same type is silently dropped \
         at merge. Add them to \
         crates/core/src/validation_bound.rs::SUPPORTED_ASSERTION_TYPES (with a \
         matching validate_bound_check_shape arm)."
    );

    // Sanity floor: the extraction must find a plausible number of arms. A
    // refactor that renames the dispatch variable or flattens the match would
    // otherwise reduce both sets toward empty and pass vacuously.
    assert!(
        implemented_set.len() >= 14,
        "extracted only {} assertion-type arms from `run_assertion` — extraction \
         is probably broken: {implemented:?}",
        implemented_set.len()
    );
}

/// The shipped archetype contracts' `.schema.json` `assertion_type` enums are a
/// third, independent copy of the same list. Pin them too: a type absent from a
/// contract schema cannot appear in an SME bound merged into a contract that
/// carries that sidecar, because `emit` schema-validates the MERGED document.
#[test]
fn contract_schema_enums_cover_the_sme_whitelist() {
    use ecaa_workflow_core::validation_bound::SUPPORTED_ASSERTION_TYPES;

    let config_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../config/downstream-policy")
        .canonicalize()
        .expect("config/downstream-policy must exist relative to the harness crate");

    let mut checked = 0usize;
    let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(&config_dir)
        .expect("reading config/downstream-policy")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.starts_with("validation-contract") && n.ends_with(".schema.json")
            })
        })
        .collect();
    entries.sort();

    for path in entries {
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("reading contract schema"))
                .unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()));
        let Some(enumeration) = value
            .pointer(
                "/properties/stages/additionalProperties/properties/assertions/items/properties/assertion_type/enum",
            )
            .and_then(|v| v.as_array())
        else {
            continue;
        };
        let declared: BTreeSet<&str> = enumeration.iter().filter_map(|v| v.as_str()).collect();
        let missing: Vec<&&str> = SUPPORTED_ASSERTION_TYPES
            .iter()
            .filter(|t| !declared.contains(**t))
            .collect();
        assert!(
            missing.is_empty(),
            "{} omits harness-runnable assertion type(s) {missing:?} from its \
             assertion_type enum — an SME bound naming one is accepted by \
             validate_validation_bound and then fails schema validation of the \
             merged contract at emit.",
            path.display()
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "no validation-contract schema with an assertion_type enum was found under \
         {} — this test would pass vacuously",
        config_dir.display()
    );
}
