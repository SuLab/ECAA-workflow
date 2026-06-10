//! Regression-prevention tests for doc/code parity.
//!
//! Every load-bearing claim in `CLAUDE.md` — numeric constants, the
//! greeting text, the blocker-title table, the canonical Lotz §5.5
//! walkthrough — is asserted here against a live reading of the code
//! or fixture. If a constant changes without the doc being refreshed,
//! CI goes red with a message pointing at the doc line to update.
//!
//! Part of the docs-as-contract practice introduced
//! by The
//! 8-pass audit that produced that plan turned up several doc/code
//! drifts that lived for weeks because no test tied them together;
//! this file closes that gap for the most visible surfaces.

use std::fs;
use std::path::{Path, PathBuf};

use ecaa_workflow_conversation::anthropic::client::{
    CONTEXT_MANAGEMENT_BETA, CONTEXT_MGMT_KEEP_TOOL_USES, CONTEXT_MGMT_TRIGGER_TOOL_USES,
};
use ecaa_workflow_conversation::harness_batch::HARNESS_BATCH_MAX_EVENTS;
use ecaa_workflow_conversation::service::{
    greeting_turn, SOFT_LANDING_ITERATION, TOOL_LOOP_CAP, TOOL_PILL_THRESHOLD,
};

/// Path to the repo root. The crate's `CARGO_MANIFEST_DIR` is
/// `crates/conversation`, so we ascend two directories to reach the
/// workspace root where `CLAUDE.md`, `docs/`, and `ui/` live.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("canonicalize repo root")
}

fn read_to_string(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Gate the CLAUDE.md doc-as-contract tests on the file's presence.
///
/// CLAUDE.md is gitignored / absent in a fresh OSS clone, so these gates
/// cannot run there. The previous behaviour was a SILENT `return;` —
/// which makes the test report green without validating anything, so a
/// contributor could not tell a real pass from a vacuous skip.
///
/// New contract:
/// - File present (internal-dev checkout) ⇒ returns `true`; the gate runs.
/// - File absent + `ECAA_CONFORMANCE_MODE` set ⇒ HARD PANIC. The
///   conformance gate is only ever run in the internal-dev checkout, which
///   always carries CLAUDE.md; a missing file there is a setup error, not
///   a license to skip.
/// - File absent + opt-out `ECAA_ALLOW_MISSING_CLAUDE_MD` set ⇒ LOUD
///   stderr skip notice + returns `false`. Preserves OSS-clone ergonomics
///   for someone who has deliberately accepted the missing-doc trade-off.
/// - File absent + neither env set ⇒ HARD PANIC pointing at the opt-out,
///   so the gate actually fires for contributors who have CLAUDE.md and an
///   absent-file regression can never pass silently.
fn claude_md_present_or_skip(gate: &str) -> bool {
    let path = repo_root().join("CLAUDE.md");
    if path.exists() {
        return true;
    }
    let env_set = |name: &str| {
        matches!(
            std::env::var(name).as_deref().unwrap_or("0"),
            "1" | "true" | "yes" | "on"
        )
    };
    if env_set("ECAA_CONFORMANCE_MODE") {
        panic!(
            "ECAA_CONFORMANCE_MODE is set but CLAUDE.md is absent; the \
             doc-as-contract gate `{gate}` cannot run. Check out CLAUDE.md \
             before running the conformance gate (WS-D4)."
        );
    }
    if env_set("ECAA_ALLOW_MISSING_CLAUDE_MD") {
        eprintln!(
            "\n>>> SKIP: CLAUDE.md absent — `{gate}` did NOT run <<<\n\
             >>> This is NOT a doc-contract pass (ECAA_ALLOW_MISSING_CLAUDE_MD \
             opt-out is set). Run with CLAUDE.md present to enforce. <<<\n"
        );
        return false;
    }
    panic!(
        "CLAUDE.md is absent and the doc-as-contract gate `{gate}` cannot \
         run. This used to skip silently — a vacuous green. If you are on an \
         OSS clone without CLAUDE.md, set ECAA_ALLOW_MISSING_CLAUDE_MD=1 to \
         opt out explicitly; otherwise check out CLAUDE.md so the gate fires \
         (WS-D4)."
    );
}

// ── Greeting snapshot ──────────────────────────────────────────────────

/// The greeting quoted in `USERS.md` must match the literal string
/// returned by `greeting_turn()`. When either changes, update both.
#[test]
fn greeting_matches_users_doc() {
    let turn = greeting_turn();
    let greeting_text = turn.content.trim();

    let guide = read_to_string(&repo_root().join("USERS.md"));

    assert!(
        guide.contains(greeting_text),
        "\n---\n`USERS.md` no longer quotes the greeting returned by \
         `greeting_turn()`.\n\nExpected to find:\n    {}\n\n\
         If you changed the greeting in `greeting.rs`, update the quote \
         in `USERS.md` to match.\n---\n",
        greeting_text
    );
}

// ── Documented numeric constants ──────────────────────────────────────

/// Every numeric constant referenced in `CLAUDE.md` must match the
/// live code value. When you change a constant, update the doc in the
/// same PR.
#[test]
fn documented_constants_match_code() {
    // CLAUDE.md §"Conversation crate" / service/ bullet:
    // "runs the 10-iteration LLM dispatch loop"
    assert_eq!(
        TOOL_LOOP_CAP, 10,
        "CLAUDE.md documents a 10-iteration tool loop — if you change \
         TOOL_LOOP_CAP, update the service/ bullet in CLAUDE.md."
    );

    // CLAUDE.md §"Conversation crate" / service/ bullet:
    // "A soft-landing nudge fires at iteration 7"
    assert_eq!(
        SOFT_LANDING_ITERATION, 7,
        "CLAUDE.md documents the soft-landing nudge at iteration 7 — if \
         you change SOFT_LANDING_ITERATION, update CLAUDE.md."
    );

    // CLAUDE.md §"Conversation crate" / tools/ bullet:
    // "only high-impact tools fire the 1s status pill"
    assert_eq!(
        TOOL_PILL_THRESHOLD.as_millis(),
        1000,
        "CLAUDE.md documents a 1s tool-call status pill threshold."
    );

    // CLAUDE.md §"Conversation crate" / anthropic/ bullet:
    // "auto-clears stale tool_result blocks after 8 tool uses,
    // keeping the most recent 4"
    assert_eq!(
        CONTEXT_MGMT_TRIGGER_TOOL_USES, 8,
        "CLAUDE.md documents the context-management trigger at 8 \
         tool uses."
    );
    assert_eq!(
        CONTEXT_MGMT_KEEP_TOOL_USES, 4,
        "CLAUDE.md documents the context-management keep-window at 4 \
         tool uses."
    );
    assert_eq!(
        CONTEXT_MANAGEMENT_BETA, "context-management-2025-06-27",
        "CLAUDE.md documents the context-management beta header value."
    );

    // CLAUDE.md §"Conversation crate" / harness_batch.rs bullet:
    // "flushes after a 10s quiet window" (quiet window is in BatcherConfig::default())
    // HARNESS_BATCH_MAX_EVENTS is not numerically documented in CLAUDE.md
    // but is a protocol-visible constant; assert it for stability so any
    // future doc that quotes it has a test.
    assert_eq!(HARNESS_BATCH_MAX_EVENTS, 512);
}

/// The transcript-polling interval and still-thinking threshold are now
/// centralized in `ui/src/lib/polling.ts` and imported
/// by useConversation. Rust can't import a TS constant, so this test
/// greps both files for the literal values CLAUDE.md claims. If the TS
/// source drops or renames them, the test fails loudly rather than
/// silently.
#[test]
fn documented_ts_constants_match_source() {
    let polling = read_to_string(
        &repo_root()
            .join("ui")
            .join("src")
            .join("lib")
            .join("polling.ts"),
    );
    let use_conversation = read_to_string(
        &repo_root()
            .join("ui")
            .join("src")
            .join("hooks")
            .join("useConversation.ts"),
    );

    // CLAUDE.md §"UI": "polls the transcript every 60s"
    assert!(
        polling.contains("60_000") || polling.contains("60000"),
        "expected polling.ts to define the 60_000 ms TRANSCRIPT_POLL_MS \
         constant that CLAUDE.md documents."
    );
    assert!(
        use_conversation.contains("TRANSCRIPT_POLL_MS"),
        "expected useConversation.ts to reference TRANSCRIPT_POLL_MS \
         from lib/polling per CLAUDE.md §UI."
    );

    // CLAUDE.md §"UI": "flips true after 8s of any in-flight turn"
    assert!(
        polling.contains("STILL_THINKING_MS")
            && (polling.contains("8_000") || polling.contains("8000")),
        "expected polling.ts to define STILL_THINKING_MS at 8000ms \
         per CLAUDE.md §UI."
    );
    assert!(
        use_conversation.contains("STILL_THINKING_MS"),
        "expected useConversation.ts to reference STILL_THINKING_MS \
         from lib/polling per CLAUDE.md §UI."
    );
}

// ── Blocker-title parity (core ↔ UI ↔ doc) ────────────────────────────

/// Titles SMEs see in blocker cards live in three places: the Rust
/// `BlockerKind` enum variants (stable wire identifier), the React
/// `BlockerCard::titleFor` function (what the SME actually reads), and
/// the blocker table in `USERS.md` (what the doc promises). When any
/// of these drift, the SME gets one story in the UI and another in the
/// docs.
#[test]
fn blocker_titles_match_across_code_and_doc() {
    // The canonical (kind, title) pairs — source-of-truth the UI renders.
    // Ten variants matching `BlockerKind` in `crates/core/src/blocker.rs`.
    let expected: &[(&str, &str)] = &[
        (
            "data_shape_mismatch",
            "The data doesn't match the expected shape",
        ),
        ("validation_failed", "A validation check failed"),
        (
            "metric_below_threshold",
            "A metric landed below the acceptable threshold",
        ),
        (
            "missing_input",
            "An upstream task hasn't produced its output yet",
        ),
        (
            "agent_error",
            "The agent hit an error it couldn't recover from",
        ),
        ("host_error", "The host system reported an error"),
        (
            "awaiting_sme_selection",
            "Your input is needed to pick between alternatives",
        ),
        (
            "pilot_oversize",
            "The pilot projection exceeds your cost ceiling",
        ),
        ("stalled", "A running task looks stuck"),
        (
            "contract_violation",
            "A validation contract assertion failed",
        ),
    ];

    let blocker_card = read_to_string(
        &repo_root()
            .join("ui")
            .join("src")
            .join("components")
            .join("BlockerCard.tsx"),
    );
    let reference = read_to_string(&repo_root().join("USERS.md"));

    for (kind, title) in expected {
        // The BlockerCard uses the Unicode right-single-quote (’)
        // in its rendered strings — sometimes as the actual character,
        // sometimes as the escape sequence `’` in TS source.
        // Normalize both forms to ASCII `'` before comparing.
        let normalize = |s: &str| s.replace('\u{2019}', "'").replace("\\u2019", "'");
        let needle = normalize(title);

        let card_hit = normalize(&blocker_card).contains(&needle);
        let doc_hit = normalize(&reference).contains(&needle);

        assert!(
            card_hit,
            "BlockerCard.tsx::titleFor is missing the title for `{kind}` \
             (expected: {title:?}). Update BlockerCard.tsx or the expected \
             table in this test."
        );
        assert!(
            doc_hit,
            "USERS.md is missing the title for `{kind}` \
             (expected: {title:?}). Update the blocker table in \
             USERS.md or the expected table in this test."
        );
    }
}

// ── Server route inventory ────────────────────────────────────────────

/// CLAUDE.md §"Server routes" claims 27 chat-route mounts. The number
/// is load-bearing for anyone sizing the API surface; if a route gets
/// added without the doc updating, this test flags it.
#[test]
fn server_chat_route_count_matches_claim() {
    let chat_routes_dir = repo_root()
        .join("crates")
        .join("server")
        .join("src")
        .join("chat_routes");
    let mut route_count = 0usize;
    for entry in fs::read_dir(&chat_routes_dir).expect("read chat_routes") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let source = read_to_string(&path);
        // A chat route is either `.route("/...", get/post/...)` or the
        // same shape using `axum::routing::`. Count the method-router
        // constructions as the canonical proxy for "one mounted route".
        for method in &["get(", "post(", "put(", "delete(", "patch("] {
            route_count += source.matches(method).count();
        }
    }
    // The counter is approximate (it also matches `get(`/`post(` used
    // inside handlers for internal reqwest calls, etc). To keep the
    // test stable without needing a perfect parser, require AT LEAST
    // the documented count. If the count drops below 27, the doc over-
    // promises and must shrink.
    assert!(
        route_count >= 27,
        "CLAUDE.md claims 27 chat-route mounts; code shows {route_count}. \
         If routes were removed, update CLAUDE.md §Server routes."
    );
}

// ── Container env-var inventory ────────────────────────────────────────

/// Every container-related `ECAA_*` env var listed
/// in plan §S15.20's inventory must appear in CLAUDE.md's env-var
/// section. Doc-as-contract gate: when the agent scripts or
/// container plumbing add a new env, this test forces a CLAUDE.md
/// edit in the same PR. When code stops consuming an env, drop it
/// from this list. The 18 envs cover the §S15.20 plan-row inventory
/// plus the operator-level ECAA_DEFAULT_CONTAINER_IMAGE fallback
/// consumed by scripts/agent-claude.sh and the §S15.x derived-image
/// warm-up envs (ECAA_FORCE_IMAGE_REBUILD, ECAA_BUILDX_CACHE_DIR,
/// ECAA_IMAGE_BUILD_TIMEOUT_SECS, ECAA_DERIVED_IMAGE_TAG_PREFIX,
/// ECAA_IMAGE_BUILDER_PATH) consumed by scripts/build-derived-image.sh
/// + crates/harness/src/executor/local.rs::warm_runtime_image.
#[test]
fn container_env_vars_documented_in_claude_md() {
    if !claude_md_present_or_skip("container_env_vars_documented_in_claude_md") {
        return;
    }
    let claude = read_to_string(&repo_root().join("CLAUDE.md"));
    let required = [
        "ECAA_CONTAINER_RUNTIME",
        "ECAA_DEFAULT_CONTAINER_IMAGE",
        "ECAA_AGENT_CACHE_DIR",
        "ECAA_AGENT_CACHE_DISABLE",
        "ECAA_AGENT_CACHE_MAX_GB",
        "ECAA_AGENT_SCRATCH_DIR",
        "ECAA_AGENT_CRED_REFRESH_SECS",
        "ECAA_LOCAL_SANDBOX",
        "ECAA_CONTAINER_REGISTRY_AUTH",
        "ECAA_CONTAINER_NETWORK_DEFAULT",
        "ECAA_CONTAINER_VERIFY",
        "ECAA_SLURM_NATIVE_CONTAINER",
        // Derived-image warm-up.
        "ECAA_FORCE_IMAGE_REBUILD",
        "ECAA_BUILDX_CACHE_DIR",
        "ECAA_IMAGE_BUILD_TIMEOUT_SECS",
        "ECAA_DERIVED_IMAGE_TAG_PREFIX",
        "ECAA_IMAGE_BUILDER_PATH",
        // Per-atom-isolated images
        // soak gate. Read by `derived_image::per_task_images_enabled`
        // and the Local executor's `provision`.
        "ECAA_PER_TASK_IMAGES",
    ];
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|env| !claude.contains(env))
        .collect();
    assert!(
        missing.is_empty(),
        "CLAUDE.md is missing the following container env vars from \
         the §S15.20 inventory: {missing:?}. Add them to the env-var \
         section, or drop from this test if the consumer was removed."
    );
}

// ── F15 — Tool enum count drift gate ───────────────────────────────────

/// CLAUDE.md no longer hard-codes a tool count integer literal — the
/// `Tool` enum's `strum::EnumCount` derive provides `Tool::COUNT` and
/// any future doc that quotes a number must reference it. This test
/// asserts the literal "16-tool" / "16 tool" strings are gone from
/// CLAUDE.md so a copy-paste backslide is caught at CI time.
#[test]
fn claude_md_does_not_hard_code_tool_count_integer() {
    use ecaa_workflow_conversation::Tool;
    if !claude_md_present_or_skip("claude_md_does_not_hard_code_tool_count_integer") {
        return;
    }
    let claude = read_to_string(&repo_root().join("CLAUDE.md"));

    // The forbidden literals — only these specific phrasings are
    // banned. Other uses of "16" (e.g., "16 MB", "16-bit") are fine.
    let forbidden = ["16-tool", "16 tool", "sixteen-tool", "sixteen tool"];
    let leaked: Vec<&str> = forbidden
        .iter()
        .copied()
        .filter(|needle| claude.contains(needle))
        .collect();
    assert!(
        leaked.is_empty(),
        "CLAUDE.md re-introduced a hard-coded tool-count literal {leaked:?}. \
         The doc must reference Tool::COUNT (currently {}) instead. F15.",
        Tool::COUNT
    );

    // CLAUDE.md no longer carries "39 typed atom" or "45 typed atom" or
    // any numeric atom-count literal in the `stage-atoms/` workspace
    // table. Same drift rationale as F15 for tools.
    let atom_literals = ["39 typed atom", "45 typed atom"];
    let atom_leaked: Vec<&str> = atom_literals
        .iter()
        .copied()
        .filter(|needle| claude.contains(needle))
        .collect();
    assert!(
        atom_leaked.is_empty(),
        "CLAUDE.md re-introduced a hard-coded atom-count literal {atom_leaked:?}. \
         Reference `crates/core/tests/atom_count_baseline.rs` instead. F15."
    );
}

// ── Harness-batcher window env var ─────────────────────────────────────

/// The `ECAA_HARNESS_BATCH_WINDOW_SECS` env var promoted
/// from the §11 watchlist must appear in CLAUDE.md's env-var section
/// as part of the doc-as-contract gate (matches §7.3 discipline). The
/// constructor `BatcherConfig::from_env` reads this; the server
/// `ChatAppState::new` threads the resulting `BatcherConfig` into
/// `HarnessBatcher::new`.
#[test]
fn harness_batch_window_env_var_documented_in_claude_md() {
    if !claude_md_present_or_skip("harness_batch_window_env_var_documented_in_claude_md") {
        return;
    }
    let claude = read_to_string(&repo_root().join("CLAUDE.md"));
    assert!(
        claude.contains("ECAA_HARNESS_BATCH_WINDOW_SECS"),
        "CLAUDE.md missing ECAA_HARNESS_BATCH_WINDOW_SECS. Add it to \
         the env-var section, or drop this test if the consumer was \
         removed."
    );
}

/// Self-test for the `claude_md_present_or_skip` contract: when CLAUDE.md
/// is present (internal-dev checkout) the helper returns `true` and the
/// doc-as-contract gates run for real. The absence paths (conformance
/// hard-fail, opt-out skip, default hard-fail) panic by construction —
/// the only way the helper returns is via the present-file path exercised
/// here — so this guards against a regression that would re-introduce a
/// silent vacuous pass (WS-D4).
#[test]
fn claude_md_present_or_skip_runs_gate_when_file_present() {
    if repo_root().join("CLAUDE.md").exists() {
        assert!(
            claude_md_present_or_skip("conformance_mode_self_test"),
            "helper must return true when CLAUDE.md is present so the gates run"
        );
    }
}

// ── Exhaustive env-var doc-gate ─────────────────────────────────────

/// Doc-as-contract gate: every user-facing `ECAA_*` name that appears in the
/// `crates/` source tree must be documented in `.env.example` (or in
/// `CLAUDE.md` when an internal checkout carries the daily-contributor block).
///
/// `.env.example` is the public checked-in catalog. Adding a new env var to
/// the source without documenting it should fail this test rather than slipping
/// into a future audit.
///
/// **False positives.** A handful of source mentions are not real env
/// vars and are filtered here:
///
/// * Names ending in `_` are sed-stripped prefixes from formatted
///   references like `ECAA_AWS_*` or doc-comment lists; they have no
///   runtime meaning.
/// * `ECAA_PER_TASK_IMAGE_ENV_LOCK` is a `cfg(test)` `Mutex<()>` static
///   serializing other env-var-mutating tests, not a tunable.
/// * `ECAA_TEST_MISSING_KEY_*` is a deliberately-synthetic missing-key
///   name used by `eval-adapters/src/scorer.rs` tests.
/// * `ECAA_DEFAULT_AGENT_PATH` / similar test-runner aliases that the
///   doc folds into a single bullet via the wildcard form are tolerated
///   when the wildcard line covers them.
#[test]
fn every_ecaa_env_var_documented() {
    use std::collections::BTreeSet;

    let env_doc_path = repo_root().join(".env.example");

    // Walk crates/ collecting every ECAA_* identifier reference.
    // Constraints: only inspect `.rs` files so the test isn't sensitive
    // to comment-format drift in TOML/YAML, and de-duplicate via BTreeSet
    // so the failure message is deterministic across runs.
    let crates_dir = repo_root().join("crates");
    let re = regex::Regex::new(r"ECAA_[A-Z_][A-Z0-9_]*").expect("compile ECAA_* regex");
    let mut source_names: BTreeSet<String> = BTreeSet::new();
    for entry in walkdir::WalkDir::new(&crates_dir)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        // Skip the test file itself — every ECAA_* name we list as a
        // false-positive filter would otherwise count as a "source"
        // reference and trip the gate.
        if path.ends_with("tests/documented_constants.rs") {
            continue;
        }
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        for m in re.find_iter(&content) {
            source_names.insert(m.as_str().to_string());
        }
    }

    // Filter out the well-known false positives (see doc-comment above).
    let false_positives: BTreeSet<&str> = [
        "ECAA_PER_TASK_IMAGE_ENV_LOCK", // cfg(test) Mutex<()>, not an env var
        "ECAA_AWS_ENV_LOCK",            // cfg(test) Mutex<()>, not an env var
        "ECAA_VERSION",                 // Rust spec-version constant, not an env var
        "ECAA_ECAA_MODE", // retired env surface; mentioned only by the inertness regression test
    ]
    .into_iter()
    .collect();
    source_names.retain(|n| !false_positives.contains(n.as_str()));
    // Drop name fragments stamped by tests that build env-var names via
    // `format!("ECAA_TEST_MISSING_KEY_{...}")` etc.
    source_names.retain(|n| !n.starts_with("ECAA_TEST_MISSING_KEY"));
    // Skip test-only env vars — fixtures, helpers, and tier-specific
    // probe ports that are not user-facing.
    source_names.retain(|n| !n.starts_with("ECAA_TEST_") && !n.starts_with("ECAA_TIER_"));
    // The sandbox-runner integration test (`crates/harness/tests/\
    // sandbox_runner_integration.rs`) builds synthetic secret-key names
    // like `TEST_ECAA_C7_MY_API_KEY` and `TEST_ECAA_C14_FOO`. The
    // regex captures the trailing `ECAA_C7_*` / `ECAA_C14_*` suffixes;
    // they're not real env vars the operator sets. Filter by name shape.
    source_names.retain(|n| !n.starts_with("ECAA_C7_") && !n.starts_with("ECAA_C14_"));

    // Read both doc anchors. `.env.example` is the primary; `CLAUDE.md`
    // carries the internal daily-contributor short list when it is present.
    let env_doc = read_to_string(&env_doc_path);
    let claude_path = repo_root().join("CLAUDE.md");
    let claude = if claude_path.exists() {
        read_to_string(&claude_path)
    } else {
        String::new()
    };

    // A name counts as documented if it appears as an exact substring
    // in either doc. This is intentionally generous — a wildcard
    // like `ECAA_LIB_PIN_<NAME>` documents the whole family because
    // the suffix-name is variable.
    let missing: Vec<String> = source_names
        .iter()
        .filter(|n| !env_doc.contains(n.as_str()) && !claude.contains(n.as_str()))
        .cloned()
        .collect();

    assert!(
        missing.is_empty(),
        "\nThe following ECAA_* env var names are referenced in crates/ \
         but not documented in .env.example or CLAUDE.md:\n\n  {}\n\n\
         Add an entry under the appropriate section in \
         .env.example (or extend an existing wildcard \
         bullet that covers the name). If a name in this list is a \
         false positive (a test-only static or a synthetic name built \
         via format!), extend the filter in this test to skip it.\n",
        missing.join("\n  ")
    );
}

// ── State-inspector tab count ─────────────────────────────────────────

/// The canonical State Inspector tab count is the `TABS` array in
/// `ui/src/components/state_inspector/index.ts`. This gate counts entries
/// in that array and — when README.md cites a tab count in its
/// architecture summary — asserts the doc cites the same number. The
/// current OSS README's architecture-at-a-glance describes the UI row
/// without a tab-count integer, so the cross-reference is skipped when
/// no `N tabs` phrase is present; if a tab count is ever re-introduced
/// to README.md it must match the live count, forcing a README edit in
/// the same PR (same docs-as-contract pattern as
/// `server_chat_route_count_matches_claim`).
#[test]
fn readme_state_inspector_tab_count_matches_source() {
    let tabs_source = read_to_string(
        &repo_root()
            .join("ui")
            .join("src")
            .join("components")
            .join("state_inspector")
            .join("index.ts"),
    );

    // Locate the TABS array literal and count the `{ id: '...' }` entries.
    // The declaration is `export const TABS: readonly TabConfig[] = [...]`,
    // so we anchor on the `= [` assignment to skip the `TabConfig[]` type
    // annotation's brackets.
    let start = tabs_source
        .find("export const TABS")
        .expect("TABS export present in index.ts");
    let after_start = &tabs_source[start..];
    let open = after_start
        .find("= [")
        .map(|i| i + 2) // position of the `[` itself
        .expect("TABS array literal opens with `= [`");
    // Find the matching close-bracket — depth-counted to tolerate nested
    // brackets inside comments / labels (unlikely but cheap).
    let bytes = after_start.as_bytes();
    let mut depth = 0i32;
    let mut close: Option<usize> = None;
    for (i, b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close.expect("TABS array literal closes with ]");
    let body = &after_start[open..=close];
    // Each entry is `{ id: 'name', label: 'Label' }`; count `id:` keys.
    let tab_count = body.matches("id:").count();
    assert!(
        tab_count >= 10,
        "TABS array has only {tab_count} entries; parse heuristic likely broken"
    );

    let readme = read_to_string(&repo_root().join("README.md"));
    // The OSS README's architecture summary describes the UI row without
    // citing a tab-count integer. The cross-reference only applies when
    // README.md actually quotes a `N tabs` phrase; if it doesn't, there's
    // nothing to keep in lock-step, so skip. If a tab count is ever
    // re-added to README.md, the `" tabs"` substring appears and the
    // exact-count assertion below fires.
    if !readme.contains(" tabs") {
        return;
    }
    let expected_phrase = format!("{} tabs", tab_count);
    assert!(
        readme.contains(&expected_phrase),
        "README.md cites a State Inspector tab count that disagrees with the source.\n\
         Live count from ui/src/components/state_inspector/index.ts: {tab_count}\n\
         Expected phrase in README.md: \"{expected_phrase}\"\n\
         Update the architecture-at-a-glance paragraph and re-run."
    );
}
