//! Enum↔SKOS-scheme agreement lint (F10 extensibility contract).
//!
//! Each closed enum's Rust variant set MUST exactly equal its registered
//! SKOS scheme's skos:notation set, and the count MUST match the pinned
//! constant. This COMPLEMENTS — does not replace — the existing pinned
//! counts (BlockerKind::COUNT==48 in blocker_variant_count.rs /
//! spec_consistency.rs; all_flags().len()==6 in core::ablation). The three
//! couplings agree by construction: Rust ⇄ COUNT, Rust ⇄ spec-md, Rust ⇄ SKOS.
//!
//! Bumping rule: adding an enum variant is a MINOR change — add the variant,
//! add a skos:Concept (with skos:notation/prefLabel/inScheme) to
//! ecaa-skos-schemes.ttl, bump that scheme's owl:versionInfo, and bump the
//! relevant pinned-count test. This lint fails until all are in lockstep.

use std::collections::BTreeSet;
use std::path::PathBuf;
use strum::EnumCount;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("CARGO_MANIFEST_DIR has two ancestors")
        .to_path_buf()
}

fn schemes_ttl() -> String {
    let p = repo_root().join("docs/ecaa-spec/registration/ecaa-skos-schemes.ttl");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {p:?}: {e}"))
}

/// CamelCase → snake_case (the serde rename_all="snake_case" wire form).
fn snake(camel: &str) -> String {
    let mut out = String::new();
    for (i, ch) in camel.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Source-parse the CamelCase variant names of `enum <enum_name> {` in
/// `crates/ecaa-types/src/<file>`, returning them as snake_case wire tokens.
/// Same brace-depth scan as spec_consistency::blocker_variant_names.
fn enum_notations(file: &str, enum_name: &str) -> BTreeSet<String> {
    let src_path = repo_root().join("crates/ecaa-types/src").join(file);
    let src =
        std::fs::read_to_string(&src_path).unwrap_or_else(|e| panic!("read {src_path:?}: {e}"));
    let re = regex::Regex::new(r"(?m)^    ([A-Z][A-Za-z0-9]+)(?:\s*\{|\s*\(|,\s*$)")
        .expect("regex compiles");
    let open = format!("pub enum {enum_name}");
    let mut in_enum = false;
    let mut depth = 0i32;
    let mut out: BTreeSet<String> = BTreeSet::new();
    for line in src.lines() {
        if !in_enum {
            if line.trim_start().starts_with(&open) {
                in_enum = true;
                depth = line.chars().filter(|&c| c == '{').count() as i32;
            }
            continue;
        }
        let opens = line.chars().filter(|&c| c == '{').count() as i32;
        let closes = line.chars().filter(|&c| c == '}').count() as i32;
        let starts_at = depth;
        depth += opens - closes;
        if depth <= 0 {
            break;
        }
        if starts_at != 1 {
            continue;
        }
        if let Some(caps) = re.captures(line) {
            out.insert(snake(&caps[1]));
        }
    }
    out
}

/// Parse `skos:notation "<tok>"` strings that belong to the given scheme.
/// Each concept block opens at `<…/<scheme>/<tok>> a skos:Concept` and
/// carries `skos:inScheme vocab:<scheme>` + `skos:notation "<tok>"`. We
/// collect every notation whose concept declares inScheme this scheme.
fn scheme_notations(ttl: &str, scheme: &str) -> BTreeSet<String> {
    let in_scheme = format!("skos:inScheme vocab:{scheme}");
    let mut out: BTreeSet<String> = BTreeSet::new();
    // Concept blocks are separated by the `a skos:Concept` type assertion.
    for block in ttl.split("a skos:Concept") {
        if !block.contains(&in_scheme) {
            continue;
        }
        // Pull the skos:notation "…" from this block.
        if let Some(idx) = block.find("skos:notation") {
            let rest = &block[idx..];
            if let (Some(a), Some(b)) = (
                rest.find('"'),
                rest[rest.find('"').unwrap() + 1..].find('"'),
            ) {
                let start = a + 1;
                let tok = &rest[start..start + b];
                out.insert(tok.to_string());
            }
        }
    }
    out
}

#[test]
fn blocker_kind_enum_matches_scheme() {
    let rust = enum_notations("blocker.rs", "BlockerKind");
    let skos = scheme_notations(&schemes_ttl(), "blocker-kind");
    assert_eq!(
        rust.len(),
        48,
        "source-parsed BlockerKind variant count drifted from 48 (sync the SKOS scheme + pinned tests)"
    );
    // Cross-check the strum::EnumCount pin so all three couplings agree.
    assert_eq!(ecaa_workflow_core::blocker::BlockerKind::COUNT, 48);
    assert_eq!(
        rust, skos,
        "BlockerKind variants ≠ blocker-kind SKOS notations.\n  only in Rust: {:?}\n  only in SKOS: {:?}",
        rust.difference(&skos).collect::<Vec<_>>(),
        skos.difference(&rust).collect::<Vec<_>>()
    );
}

#[test]
fn rerun_outcome_enum_matches_scheme() {
    let rust = enum_notations("reexecution.rs", "ReexecutionBucket");
    let skos = scheme_notations(&schemes_ttl(), "rerun-outcome");
    assert_eq!(
        rust.len(),
        5,
        "ReexecutionBucket (RerunOutcome.class) must be 5 variants"
    );
    assert_eq!(
        rust, skos,
        "ReexecutionBucket variants ≠ rerun-outcome SKOS notations\n  rust-only: {:?}\n  skos-only: {:?}",
        rust.difference(&skos).collect::<Vec<_>>(),
        skos.difference(&rust).collect::<Vec<_>>()
    );
}

#[test]
fn ablation_flag_enum_matches_scheme() {
    let rust = enum_notations("ablation.rs", "AblationFlag");
    let skos = scheme_notations(&schemes_ttl(), "ablation-flag");
    assert_eq!(rust.len(), 6, "AblationFlag must be 6 variants");
    // Cross-check all_flags() — the canonical runtime pin.
    assert_eq!(ecaa_workflow_core::ablation::all_flags().len(), 6);
    assert_eq!(
        rust, skos,
        "AblationFlag variants ≠ ablation-flag SKOS notations\n  rust-only: {:?}\n  skos-only: {:?}",
        rust.difference(&skos).collect::<Vec<_>>(),
        skos.difference(&rust).collect::<Vec<_>>()
    );
}

#[test]
fn scheme_versioninfo_pins_match_counts() {
    let ttl = schemes_ttl();
    // owl:versionInfo on each scheme must equal the variant count.
    for (scheme, count) in [
        ("blocker-kind", "48"),
        ("rerun-outcome", "5"),
        ("ablation-flag", "6"),
    ] {
        let head = ttl
            .split(&format!("vocab:{scheme} a skos:ConceptScheme"))
            .nth(1)
            .unwrap_or_else(|| panic!("scheme vocab:{scheme} not found in ttl"));
        let upto = &head[..head.find("a skos:Concept").unwrap_or(head.len())];
        assert!(
            upto.contains(&format!("owl:versionInfo \"{count}\"")),
            "scheme vocab:{scheme} must carry owl:versionInfo \"{count}\""
        );
    }
}
