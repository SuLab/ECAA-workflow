//! Standalone end-of-run package finalization.
//!
//! The server finalizes per-task on each `task_completed` event
//! (`core::finalize::finalize_task` via `verification::reverify_and_block_on_mismatch`).
//! A standalone harness run (`--no-interactive`, no `--session-id`) sends no
//! session events, so nothing finalizes the package: the signed verdict sink is
//! never written, the plaintext `runtime/claim-verification.json` stays an empty
//! emit-time stub, evidence is unregistered, and the at-rest audit-proof falls
//! back to the vacuous emit stub.
//!
//! This module sources the finalize inputs from the SELF-CONTAINED emitted
//! package (plus a host-resolved `config_dir` for the base interpretation
//! policy + extractor config, and `ECAA_AUDIT_SECRET` for the HMAC sink) and
//! calls [`ecaa_workflow_core::finalize::finalize_package`] once at the end of a
//! completed run. It is intentionally a thin, testable shim around the core
//! orchestration: no session, no HTTP, no state transition.
//!
//! Failure is NON-FATAL — every error path here logs and returns so the caller
//! can still truncate the WAL and exit `Ok(())`. Finalizing a package is a
//! best-effort post-exec convenience, never a gate.

use ecaa_workflow_core::decision_log::DecisionRecord;
use ecaa_workflow_core::finalize::{finalize_package, PackageFinalizeSummary};
use ecaa_workflow_core::project_class::ProjectClass;
use std::path::{Path, PathBuf};

/// Stage stems that mark a run as confirmatory (DE / differential-accessibility
/// / variant-calling / a clinical primary-endpoint). A package whose DAG names
/// any of these is treated as confirmatory for the finalize call. Pre-Task-5
/// `decisions` is empty, so `is_confirmatory` is provably inert today (it only
/// gates `demote_claims_from_deviations`, which needs `PostHocDeviation`
/// records); the heuristic is implemented so it is correct once Task 5 starts
/// populating `runtime/decisions.jsonl`.
const CONFIRMATORY_STAGE_STEMS: &[&str] = &[
    "differential_expression",
    "differential_accessibility",
    "variant_calling",
    "primary_endpoint",
];

/// Derive the 32-byte HMAC key from `ECAA_AUDIT_SECRET`, byte-identically to
/// how `ecaa-workflow-audit-proof` derives its `--secret`/`ECAA_AUDIT_SECRET`
/// key (`crates/cli/src/bin/ecaa-workflow-audit-proof.rs::writer_from_hex`):
/// hex-decode the trimmed string and require EXACTLY 32 bytes (64 hex chars).
///
/// Matching that derivation is load-bearing: Task 9 later runs
/// `ecaa-workflow-audit-proof --secret "$ECAA_AUDIT_SECRET"` to VERIFY the HMAC
/// this harness wrote. A loose derivation (e.g. raw UTF-8 bytes on hex failure)
/// would sign with a key the verifier can never reproduce.
///
/// Returns `None` when the env var is unset/empty, OR when it is not a valid
/// 64-hex-char string — in the latter case the harness logs a warning and skips
/// the signed sink rather than signing with a key the audit-proof tool would
/// reject. (Passing `None` to `finalize_package` leaves audit-proof Inv 1/5
/// Unverified, which is the documented degraded mode.)
pub fn audit_secret_from_env() -> Option<[u8; 32]> {
    let raw = std::env::var("ECAA_AUDIT_SECRET").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    match hex::decode(trimmed) {
        Ok(bytes) => match <[u8; 32]>::try_from(bytes.as_slice()) {
            Ok(key) => Some(key),
            Err(_) => {
                tracing::warn!(
                    target: "harness-finalize",
                    got_bytes = bytes.len(),
                    "ECAA_AUDIT_SECRET decoded to {} byte(s), not 32 (64 hex chars) — \
                     signed verdict sink not written; set a 64-hex-char secret to match \
                     ecaa-workflow-audit-proof --secret",
                    bytes.len()
                );
                None
            }
        },
        Err(e) => {
            tracing::warn!(
                target: "harness-finalize",
                error = %e,
                "ECAA_AUDIT_SECRET is not valid hex — signed verdict sink not written; \
                 it must be the 64-hex-char per-session secret that \
                 ecaa-workflow-audit-proof --secret reads"
            );
            None
        }
    }
}

/// Resolve the config directory carrying `downstream-policy/interpretation-policy.json`
/// (the BASE policy + extractor config the finalize path reads). Mirrors the
/// server's `chat_routes::tasks::config_dir_or_default` resolution so a
/// standalone harness launched from an arbitrary CWD finds the same policy:
///
/// 1. `ECAA_CONFIG_DIR` — explicit operator override, always wins.
/// 2. Binary-relative discovery — walk up from `current_exe()` for a `config/`
///    dir carrying the `downstream-policy` marker (works for an installed
///    `ecaa-workflow-harness`).
/// 3. CWD-relative `config` — final fallback (repo-root / test launches).
///
/// NOTE: this is the BASE policy directory, distinct from the package's own
/// `policies/interpretation-policy.json` (the injected expected-claim manifest),
/// which `core::finalize` reads separately via the package root.
pub fn resolve_config_dir() -> PathBuf {
    if let Ok(explicit) = std::env::var("ECAA_CONFIG_DIR") {
        return PathBuf::from(explicit);
    }
    if let Some(found) = config_dir_from_exe() {
        return found;
    }
    PathBuf::from("config")
}

fn config_dir_has_marker(dir: &Path) -> bool {
    dir.join("downstream-policy").is_dir()
}

fn config_dir_from_exe() -> Option<PathBuf> {
    config_dir_from_exe_path(&std::env::current_exe().ok()?)
}

/// Pure walk-up over a given executable path (split out for testing — the real
/// `current_exe()` can't be relocated under `cargo test`).
fn config_dir_from_exe_path(exe: &Path) -> Option<PathBuf> {
    let mut dir = exe.parent();
    while let Some(d) = dir {
        let candidate = d.join("config");
        if config_dir_has_marker(&candidate) {
            return Some(candidate);
        }
        dir = d.parent();
    }
    None
}

/// Read `runtime/decisions.jsonl` back into `Vec<DecisionRecord>`. Line-delimited
/// JSON; a malformed line is skipped (whole-run warned) rather than aborting.
/// Returns an empty vec when the file is absent or empty — the correct value for
/// a standalone run until Task 5 starts populating the log.
pub fn load_decisions(package_root: &Path) -> Vec<DecisionRecord> {
    let path = package_root.join("runtime").join("decisions.jsonl");
    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    let mut skipped = 0usize;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<DecisionRecord>(line) {
            Ok(rec) => out.push(rec),
            Err(_) => skipped += 1,
        }
    }
    if skipped > 0 {
        tracing::warn!(
            target: "harness-finalize",
            skipped,
            "skipped {} malformed line(s) in runtime/decisions.jsonl",
            skipped
        );
    }
    out
}

/// Derive `is_confirmatory` from the package's DAG: true when any task's id or
/// `source_atom_id` stem matches a [`CONFIRMATORY_STAGE_STEMS`] entry. The
/// package carries no session, so this is the package-derivable analog of the
/// server's `session.mode.is_confirmatory()`. Reads `WORKFLOW.json`; returns
/// `false` when it is absent/unparsable (the conservative default).
pub fn derive_is_confirmatory(package_root: &Path) -> bool {
    let Ok(bytes) = std::fs::read(package_root.join("WORKFLOW.json")) else {
        return false;
    };
    let Ok(wf) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    let Some(tasks) = wf.get("tasks").and_then(|t| t.as_object()) else {
        return false;
    };
    let is_confirmatory_stem = |s: &str| {
        CONFIRMATORY_STAGE_STEMS
            .iter()
            .any(|stem| s.contains(stem))
    };
    tasks.iter().any(|(task_id, t)| {
        if is_confirmatory_stem(task_id) {
            return true;
        }
        t.get("source_atom_id")
            .and_then(|v| v.as_str())
            .map(is_confirmatory_stem)
            .unwrap_or(false)
    })
}

/// Source every input from the self-contained package (+ host config_dir + env
/// secret) and run [`finalize_package`] once. The whole thing is best-effort:
/// any failure is logged and `Ok(())` is returned so the caller still truncates
/// the WAL and exits cleanly. This is the unit-testable extraction of the
/// harness end-of-run finalize that `run_loop` calls inside its
/// `after.is_complete()` block.
///
/// `config_dir` is passed in explicitly (rather than resolved here) so the
/// caller can resolve it once at startup and the test can point it at the
/// repo's real config tree.
pub fn finalize_completed_package(package_root: &Path, config_dir: &Path) {
    let secret = audit_secret_from_env();
    let project_class = ProjectClass::default();
    let is_confirmatory = derive_is_confirmatory(package_root);
    let decisions = load_decisions(package_root);

    match finalize_package(
        package_root,
        config_dir,
        project_class,
        &decisions,
        is_confirmatory,
        secret.as_ref(),
    ) {
        Ok(PackageFinalizeSummary {
            tasks_finalized,
            coverage_gaps,
        }) => {
            println!(
                "  finalized {} task(s); {} coverage gap(s)",
                tasks_finalized,
                coverage_gaps.len()
            );
            for gap in &coverage_gaps {
                tracing::warn!(target: "harness-finalize", gap = %gap, "coverage gap");
            }
            if secret.is_none() {
                tracing::warn!(
                    target: "harness-finalize",
                    "ECAA_AUDIT_SECRET unset/invalid — signed verdict sink not written; \
                     audit-proof Inv 1/5 stay Unverified"
                );
            }
        }
        Err(e) => {
            tracing::warn!(target: "harness-finalize", error = %e, "package finalize failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_secret_requires_exactly_64_hex_chars() {
        // 64 hex chars → 32 bytes → Some.
        std::env::set_var("ECAA_AUDIT_SECRET", "ab".repeat(32));
        assert!(audit_secret_from_env().is_some(), "64 hex chars must decode");
        // Wrong length (62 chars) → None, not a loose fallback.
        std::env::set_var("ECAA_AUDIT_SECRET", "ab".repeat(31));
        assert!(
            audit_secret_from_env().is_none(),
            "31 bytes must be rejected (no raw-bytes fallback)"
        );
        // Non-hex → None.
        std::env::set_var("ECAA_AUDIT_SECRET", "not-hex-at-all-zz");
        assert!(audit_secret_from_env().is_none(), "non-hex must be None");
        // Unset → None.
        std::env::remove_var("ECAA_AUDIT_SECRET");
        assert!(audit_secret_from_env().is_none(), "unset must be None");
    }

    #[test]
    fn derive_is_confirmatory_detects_de_stage() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("WORKFLOW.json"),
            r#"{"tasks":{"differential_expression":{"source_atom_id":"differential_expression"}}}"#,
        )
        .unwrap();
        assert!(derive_is_confirmatory(tmp.path()));
    }

    #[test]
    fn derive_is_confirmatory_false_for_non_confirmatory_dag() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("WORKFLOW.json"),
            r#"{"tasks":{"alignment":{"source_atom_id":"alignment"}}}"#,
        )
        .unwrap();
        assert!(!derive_is_confirmatory(tmp.path()));
        // Absent WORKFLOW.json → conservative false.
        let empty = tempfile::tempdir().unwrap();
        assert!(!derive_is_confirmatory(empty.path()));
    }

    #[test]
    fn load_decisions_absent_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load_decisions(tmp.path()).is_empty());
    }

    #[test]
    fn load_decisions_skips_malformed_lines() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("runtime")).unwrap();
        // One blank, one malformed, no valid → empty but no panic.
        std::fs::write(
            tmp.path().join("runtime").join("decisions.jsonl"),
            "\n{ not json \n",
        )
        .unwrap();
        assert!(load_decisions(tmp.path()).is_empty());
    }

    #[test]
    fn config_dir_walk_up_finds_marked_config() {
        let tmp = tempfile::tempdir().unwrap();
        // <tmp>/bin/harness ; marker at <tmp>/config/downstream-policy.
        std::fs::create_dir_all(tmp.path().join("config").join("downstream-policy")).unwrap();
        std::fs::create_dir_all(tmp.path().join("bin")).unwrap();
        let exe = tmp.path().join("bin").join("ecaa-workflow-harness");
        let found = config_dir_from_exe_path(&exe).expect("walk-up should find config");
        assert_eq!(found, tmp.path().join("config"));
        // No marker anywhere → None.
        let bare = tempfile::tempdir().unwrap();
        assert!(config_dir_from_exe_path(&bare.path().join("bin").join("h")).is_none());
    }
}
