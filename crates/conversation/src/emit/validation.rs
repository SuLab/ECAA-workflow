//! Emit-time validation of ECAA sidecars against the machine-readable
//! companions in `docs/ecaa-spec/`.
//!
//! Runs every emit (when enabled). Two layers:
//!
//! 1. **Pure-Rust JSON Schema validation** — runs by default. Each
//!    emitted sidecar is checked against its embedded JSON Schema
//!    (draft-07). Schema files are bundled into the binary via
//!    `include_str!`. Fast (~ms per sidecar), no runtime deps.
//!
//! 2. **External Python validators** — opt-in via
//!    `SWFC_VALIDATE_ON_EMIT=full`. Invokes
//!    `scripts/spec-check/project_package.py` (RDF projection → SHACL
//!    via pyshacl), `owl_consistency.py` (HermiT DL satisfiability via
//!    owlready2), and `runcrate validate` (WRROC v0.5 round-trip).
//!    Gracefully degrades when Python deps or scripts are missing —
//!    missing tooling is reported as `Unavailable`, not `Fail`.
//!
//! All results aggregate into `runtime/validation-summary.json`.
//! Warn-only by default: validation failure NEVER blocks `emit_package`
//! unless `SWFC_VALIDATION_BLOCK_ON_FAIL=1` is set.
//!
//! # Environment variables
//!
//! - `SWFC_VALIDATE_ON_EMIT`
//!   - unset / `schema_only` (default, sane for production) — Pure-Rust JSON Schema only.
//!   - `full` — Schema + external Python validators.
//!   - `off` / `0` / `false` / `no` — Skip all validation (escape hatch).
//! - `SWFC_SPEC_SCRIPTS_DIR` (default: auto-detect from CARGO_MANIFEST_DIR)
//!   Override directory containing `project_package.py` and
//!   `owl_consistency.py`. Auto-detect resolves
//!   `<crate>/../../scripts/spec-check/`.
//! - `SWFC_VALIDATION_BLOCK_ON_FAIL` (default `0`, warn-only)
//!   When `1`/`true`/`yes`, schema-validation failures cause
//!   `validate_emitted_package` to return `Err`, aborting the emit.
//! - `SWFC_VALIDATION_EXTERNAL_TIMEOUT_SECS` (default `30`)
//!   Per-subprocess timeout for external Python validators.

use anyhow::{anyhow, Result};
use jsonschema::JSONSchema;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

// 8 embedded JSON Schemas. Bundled at compile time so the binary needs
// no spec files on disk at runtime.
const INTENT_SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/ecaa-spec/subgraph-schemas/intent.schema.json"
));
const DECISION_SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/ecaa-spec/subgraph-schemas/decision.schema.json"
));
const EXECUTION_SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/ecaa-spec/subgraph-schemas/execution.schema.json"
));
const EVIDENCE_SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/ecaa-spec/subgraph-schemas/evidence.schema.json"
));
const CLAIM_SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/ecaa-spec/subgraph-schemas/claim.schema.json"
));
const EQUIVALENCE_SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/ecaa-spec/subgraph-schemas/equivalence.schema.json"
));
const FAILURE_SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/ecaa-spec/subgraph-schemas/failure.schema.json"
));
const AUDIT_PROOF_SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/ecaa-spec/subgraph-schemas/audit-proof.schema.json"
));

/// One emit-validation pass's aggregated result. Serialized to
/// `runtime/validation-summary.json`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ValidationSummary {
    /// Schema version of the `validation-summary.json` file (e.g. `"0.1"`).
    pub schema_version: String,
    /// Which validation tier was executed.
    pub mode: ValidationMode,
    /// Results of the pure-Rust JSON Schema validation pass.
    pub schema_validation: SchemaValidationResults,
    /// Results of the external (Python) validators; `None` when mode is
    /// `SchemaOnly` or `Disabled`.
    pub external_validation: Option<ExternalValidationResults>,
    /// Wall-clock duration of the full validation pass in milliseconds.
    pub duration_ms: u128,
}

/// Effective validation level. Derived from `SWFC_VALIDATE_ON_EMIT`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ValidationMode {
    /// Skip all validation (`SWFC_VALIDATE_ON_EMIT=off`).
    Disabled,
    /// Pure-Rust JSON Schema validation only (default, or
    /// `SWFC_VALIDATE_ON_EMIT=schema_only`).
    SchemaOnly,
    /// Schema + external Python validators (`SWFC_VALIDATE_ON_EMIT=full`).
    Full,
}

/// Aggregate result of the JSON Schema validation pass.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SchemaValidationResults {
    /// Number of sidecar files that passed schema validation.
    pub passed: usize,
    /// Sidecar files that failed schema validation.
    pub failed: Vec<SchemaFailure>,
    /// Count of sidecars whose `SidecarSource` is `HarnessRuntime` and
    /// which the validator therefore skipped at emit time. These files
    /// are not expected to exist until the harness writes them post-emit;
    /// they are not failures and never block emission, but operators
    /// reading `validation-summary.json` can tell a skip apart from a
    /// pass. Defaults to 0 for older summaries via serde.
    #[serde(default)]
    pub skipped_pending_harness: usize,
}

/// One sidecar that failed JSON Schema validation.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SchemaFailure {
    /// Relative path of the sidecar file inside the package.
    pub sidecar: String,
    /// Zero-based line index for JSONL sidecars; `None` for single-object files.
    pub line_index: Option<usize>,
    /// Validation error message.
    pub error: String,
}

/// Results of the external (Python) validator suite.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExternalValidationResults {
    /// Outcome of the SHACL projection check against the ECAA graph.
    pub shacl_projection: ExternalCheckOutcome,
    /// Outcome of the OWL consistency check.
    pub owl_consistency: ExternalCheckOutcome,
    /// Outcome of the `runcrate validate` conformance check.
    pub runcrate_validate: ExternalCheckOutcome,
}

/// Outcome of a single external (Python) validation check.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ExternalCheckOutcome {
    /// External tool ran and reported the package conformant.
    Pass {
        /// Human-readable details from the tool's stdout/report.
        details: String,
    },
    /// External tool ran and reported a violation.
    Fail {
        /// Human-readable description of the violation.
        details: String,
    },
    /// External tool not available (Python dep missing, script absent, etc.).
    Unavailable {
        /// Reason the tool could not be invoked.
        reason: String,
    },
    /// External tool invocation errored before producing a verdict.
    Error {
        /// Error message from the failed invocation.
        reason: String,
    },
}

/// Where a sidecar is produced. The validator skips harness-runtime
/// sidecars at emit time so a missing-but-expected file doesn't block
/// emission under `SWFC_VALIDATION_BLOCK_ON_FAIL=1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidecarSource {
    /// Written by `emit_with_conversation_log` (always present after emit).
    EmitTime,
    /// Written by the harness post-emit as tasks execute. At emit time
    /// these are expected to be absent — the validator records them as
    /// `skipped_pending_harness` instead of `failed`.
    HarnessRuntime,
}

/// Catalog row: `(sidecar relative path, JSONL?, embedded schema JSON, source)`.
fn sidecar_schemas() -> [(&'static str, bool, &'static str, SidecarSource); 8] {
    [
        (
            "runtime/intake-conversation.jsonl",
            true,
            INTENT_SCHEMA,
            SidecarSource::EmitTime,
        ),
        (
            "runtime/decisions.jsonl",
            true,
            DECISION_SCHEMA,
            SidecarSource::EmitTime,
        ),
        // Subgraph E: written by harness task runners as validation
        // obligations fire post-emit. Not present at compile time.
        (
            "runtime/validation-reports.jsonl",
            true,
            EXECUTION_SCHEMA,
            SidecarSource::HarnessRuntime,
        ),
        (
            "runtime/proofs.jsonl",
            true,
            EVIDENCE_SCHEMA,
            SidecarSource::EmitTime,
        ),
        (
            "runtime/claim-verification.json",
            false,
            CLAIM_SCHEMA,
            SidecarSource::EmitTime,
        ),
        // Subgraph Q: typed verifier-decision event log written by the
        // harness verifier as tasks complete. Not present at compile time.
        (
            "runtime/verifier-decisions.jsonl",
            true,
            EQUIVALENCE_SCHEMA,
            SidecarSource::HarnessRuntime,
        ),
        (
            "runtime/assumptions.jsonl",
            true,
            FAILURE_SCHEMA,
            SidecarSource::EmitTime,
        ),
        (
            "runtime/audit-proof-report.json",
            false,
            AUDIT_PROOF_SCHEMA,
            SidecarSource::EmitTime,
        ),
    ]
}

fn ablated_sidecar(relpath: &str) -> bool {
    use ecaa_workflow_core::ablation::{AblationFlag, AblationFlagExt};
    match relpath {
        "runtime/decisions.jsonl" => AblationFlag::DecisionRecords.is_active(),
        "runtime/audit-proof-report.json" => AblationFlag::AuditProof.is_active(),
        _ => false,
    }
}

/// Process-wide cache of compiled JSON Schemas keyed by sidecar
/// relative path. Each schema compiles exactly once per process. The
/// `JSONSchema` validator is `Sync` so concurrent emits share the same
/// compiled artifact without serialization.
static SCHEMA_CACHE: Lazy<HashMap<&'static str, JSONSchema>> = Lazy::new(|| {
    let mut map = HashMap::new();
    for (relpath, _is_jsonl, schema_src, _source) in sidecar_schemas() {
        let schema_value: Value = match serde_json::from_str(schema_src) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(
                    "[ecaa-validation] embedded schema {} parse error at compile-cache init: {} (sidecar will be reported as schema-parse failure on every emit)",
                    relpath,
                    e
                );
                continue;
            }
        };
        match JSONSchema::compile(&schema_value) {
            Ok(v) => {
                map.insert(relpath, v);
            }
            Err(e) => {
                tracing::error!(
                    "[ecaa-validation] embedded schema {} compile error at cache init: {}",
                    relpath,
                    e
                );
            }
        }
    }
    map
});

/// Read `SWFC_VALIDATE_ON_EMIT`. Defaults to `SchemaOnly` for sane
/// production behavior — pure-Rust validation only, no Python dep.
/// Local dev / CI sets `=full` to enable the external validators.
fn read_mode() -> ValidationMode {
    let raw = std::env::var("SWFC_VALIDATE_ON_EMIT").unwrap_or_default();
    let mode = match raw.as_str() {
        "off" | "0" | "false" | "no" => ValidationMode::Disabled,
        "full" => ValidationMode::Full,
        "schema_only" | "" => ValidationMode::SchemaOnly,
        other => {
            tracing::warn!(
                value = ?other,
                "[ecaa-validation] unknown SWFC_VALIDATE_ON_EMIT; falling back to schema_only"
            );
            ValidationMode::SchemaOnly
        }
    };
    tracing::debug!(mode = ?mode, "[ecaa-validation] mode resolved");
    mode
}

fn read_block_on_fail() -> bool {
    matches!(
        std::env::var("SWFC_VALIDATION_BLOCK_ON_FAIL")
            .as_deref()
            .unwrap_or("0"),
        "1" | "true" | "yes" | "on"
    )
}

fn read_external_timeout() -> Duration {
    let secs = std::env::var("SWFC_VALIDATION_EXTERNAL_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(30);
    Duration::from_secs(secs)
}

/// Resolve the path to `scripts/spec-check/` relative to a known anchor.
/// Order: `SWFC_SPEC_SCRIPTS_DIR` env var, then
/// `CARGO_MANIFEST_DIR/../../scripts/spec-check/`.
fn spec_scripts_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SWFC_SPEC_SCRIPTS_DIR") {
        let pb = PathBuf::from(p);
        if pb.is_dir() {
            tracing::debug!(path = %pb.display(), "[ecaa-validation] scripts dir from env");
            return Some(pb);
        }
        tracing::warn!(
            path = %pb.display(),
            "[ecaa-validation] SWFC_SPEC_SCRIPTS_DIR not a directory; falling back to auto-detect"
        );
    }
    let default = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/spec-check")
        .canonicalize()
        .ok();
    let resolved = default.filter(|p| p.is_dir());
    match resolved.as_ref() {
        Some(p) => tracing::debug!(path = %p.display(), "[ecaa-validation] scripts dir auto-detected"),
        None => tracing::warn!(
            "[ecaa-validation] scripts/spec-check/ not found via auto-detect; external validators will be Unavailable"
        ),
    }
    resolved
}

/// Run pure-Rust JSON Schema validation on every required sidecar.
fn validate_schemas_pure_rust(pkg_root: &Path) -> SchemaValidationResults {
    let mut passed = 0usize;
    let mut failed: Vec<SchemaFailure> = Vec::new();
    let mut skipped_pending_harness = 0usize;
    tracing::debug!(
        pkg = %pkg_root.display(),
        "[ecaa-validation] pure-Rust schema validation begin"
    );

    for (relpath, is_jsonl, _schema_src, source) in sidecar_schemas() {
        // Harness-runtime sidecars (subgraph E execution + subgraph Q
        // verifier-decisions) are written by tasks executing post-emit.
        // Skip them here so a clean compile-time package isn't blocked
        // by their absence under BLOCK_ON_FAIL=1. The harness itself
        // re-checks these files via its own validator once they exist.
        if matches!(source, SidecarSource::HarnessRuntime) {
            skipped_pending_harness += 1;
            tracing::debug!(
                relpath = %relpath,
                "[ecaa-validation] sidecar skipped (harness-runtime)"
            );
            continue;
        }
        let abs = pkg_root.join(relpath);
        if !abs.exists() {
            if ablated_sidecar(relpath) {
                tracing::debug!(
                    relpath = %relpath,
                    "[ecaa-validation] sidecar skipped (ablated by SWFC_ABLATE_* flag)"
                );
                continue;
            }
            tracing::warn!(relpath = %relpath, "[ecaa-validation] sidecar missing");
            failed.push(SchemaFailure {
                sidecar: relpath.to_string(),
                line_index: None,
                error: "sidecar missing".to_string(),
            });
            continue;
        }
        let body = match std::fs::read_to_string(&abs) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(relpath = %relpath, error = %e, "[ecaa-validation] sidecar read error");
                failed.push(SchemaFailure {
                    sidecar: relpath.to_string(),
                    line_index: None,
                    error: format!("read error: {e}"),
                });
                continue;
            }
        };
        let validator = match SCHEMA_CACHE.get(relpath) {
            Some(v) => v,
            None => {
                failed.push(SchemaFailure {
                    sidecar: relpath.to_string(),
                    line_index: None,
                    error:
                        "schema not in compile-time cache (parse/compile failed at process init)"
                            .to_string(),
                });
                continue;
            }
        };

        if is_jsonl {
            // JSONL: schemas declare `type: array, items: ...`. Wrap the
            // non-empty lines into an in-memory array and validate that
            // single Value against the schema. Per-line parse errors are
            // surfaced separately so the caller can pinpoint malformed lines.
            let mut entries: Vec<Value> = Vec::new();
            let mut parse_failed = false;
            for (idx, line) in body.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(trimmed) {
                    Ok(v) => entries.push(v),
                    Err(e) => {
                        failed.push(SchemaFailure {
                            sidecar: relpath.to_string(),
                            line_index: Some(idx),
                            error: format!("JSON parse error: {e}"),
                        });
                        parse_failed = true;
                    }
                }
            }
            if parse_failed {
                tracing::warn!(relpath = %relpath, "[ecaa-validation] sidecar failed JSONL parse");
                continue;
            }
            let array_instance = Value::Array(entries);
            let msgs: Vec<String> = match validator.validate(&array_instance) {
                Ok(()) => Vec::new(),
                Err(errors) => errors.map(|e| e.to_string()).collect(),
            };
            if msgs.is_empty() {
                passed += 1;
                tracing::debug!(relpath = %relpath, "[ecaa-validation] sidecar passed schema");
            } else {
                failed.push(SchemaFailure {
                    sidecar: relpath.to_string(),
                    line_index: None,
                    error: format!("schema validation: {}", msgs.join("; ")),
                });
                tracing::warn!(
                    relpath = %relpath,
                    "[ecaa-validation] sidecar failed schema (see validation-summary.json)"
                );
            }
        } else {
            let trimmed = body.trim();
            if trimmed.is_empty() {
                passed += 1;
                tracing::debug!(relpath = %relpath, "[ecaa-validation] sidecar passed schema (empty allowed)");
                continue;
            }
            let instance: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(e) => {
                    failed.push(SchemaFailure {
                        sidecar: relpath.to_string(),
                        line_index: None,
                        error: format!("JSON parse error: {e}"),
                    });
                    continue;
                }
            };
            let msgs: Vec<String> = match validator.validate(&instance) {
                Ok(()) => Vec::new(),
                Err(errors) => errors.map(|e| e.to_string()).collect(),
            };
            if msgs.is_empty() {
                passed += 1;
                tracing::debug!(relpath = %relpath, "[ecaa-validation] sidecar passed schema");
            } else {
                failed.push(SchemaFailure {
                    sidecar: relpath.to_string(),
                    line_index: None,
                    error: format!("schema validation: {}", msgs.join("; ")),
                });
                tracing::warn!(relpath = %relpath, "[ecaa-validation] sidecar failed schema");
            }
        }
    }

    tracing::info!(
        passed,
        failed = failed.len(),
        skipped_pending_harness,
        "[ecaa-validation] schema-validation complete"
    );
    SchemaValidationResults {
        passed,
        failed,
        skipped_pending_harness,
    }
}

/// Run an external Python validator script with timeout enforcement.
fn run_external_check(
    label: &str,
    script_name: &str,
    args: &[&str],
    scripts_dir: &Path,
    timeout: Duration,
) -> ExternalCheckOutcome {
    let script_path = scripts_dir.join(script_name);
    if !script_path.exists() {
        let reason = format!(
            "script {} not found at {}",
            script_name,
            scripts_dir.display()
        );
        tracing::debug!(label = %label, reason = %reason, "[ecaa-validation] validator unavailable");
        return ExternalCheckOutcome::Unavailable { reason };
    }
    let probe = Command::new("python3").arg("--version").output();
    if probe.is_err() {
        let reason = "python3 not on PATH".to_string();
        tracing::debug!(label = %label, reason = %reason, "[ecaa-validation] validator unavailable");
        return ExternalCheckOutcome::Unavailable { reason };
    }
    tracing::debug!(
        label = %label,
        script = %script_path.display(),
        args = ?args,
        timeout_secs = timeout.as_secs(),
        "[ecaa-validation] invoking python3 validator"
    );
    let started = Instant::now();
    let mut child = match Command::new("python3")
        .arg(&script_path)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(label = %label, error = %e, "[ecaa-validation] validator spawn error");
            return ExternalCheckOutcome::Error {
                reason: format!("subprocess spawn failed: {e}"),
            };
        }
    };
    // Poll for timeout — wait_timeout would be cleaner but adds a dep; std lib
    // approach: spin-poll try_wait every 100ms up to the timeout.
    let out = loop {
        match child.try_wait() {
            Ok(Some(_)) => match child.wait_with_output() {
                Ok(o) => break o,
                Err(e) => {
                    return ExternalCheckOutcome::Error {
                        reason: format!("wait_with_output failed: {e}"),
                    };
                }
            },
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    tracing::warn!(
                        label = %label,
                        timeout_secs = timeout.as_secs(),
                        "[ecaa-validation] validator timed out; killed"
                    );
                    return ExternalCheckOutcome::Error {
                        reason: format!(
                            "subprocess timeout after {}s (set SWFC_VALIDATION_EXTERNAL_TIMEOUT_SECS to extend)",
                            timeout.as_secs()
                        ),
                    };
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                return ExternalCheckOutcome::Error {
                    reason: format!("try_wait failed: {e}"),
                };
            }
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    tracing::debug!(
        label = %label,
        exit_code = ?out.status.code(),
        elapsed = ?started.elapsed(),
        "[ecaa-validation] validator exited"
    );
    if out.status.success() {
        tracing::info!(label = %label, details = %stdout.trim(), "[ecaa-validation] validator PASS");
        ExternalCheckOutcome::Pass {
            details: stdout.trim().to_string(),
        }
    } else {
        if stderr.contains("ModuleNotFoundError") || stdout.contains("ModuleNotFoundError") {
            let reason = "Python deps missing — install via: pip install --user --break-system-packages pyshacl pyld owlready2 rdflib jsonschema".to_string();
            tracing::warn!(label = %label, reason = %reason, "[ecaa-validation] validator unavailable (missing Python deps)");
            return ExternalCheckOutcome::Unavailable { reason };
        }
        let details = format!(
            "exit {}: {} {}",
            out.status.code().unwrap_or(-1),
            stdout.trim(),
            stderr.trim()
        );
        tracing::warn!(label = %label, details = %details, "[ecaa-validation] validator FAIL");
        ExternalCheckOutcome::Fail { details }
    }
}

/// WRROC round-trip check via the external `runcrate` Python tool.
fn run_runcrate_validate(pkg_root: &Path, timeout: Duration) -> ExternalCheckOutcome {
    let probe = Command::new("runcrate").arg("--version").output();
    match probe {
        Ok(o) if o.status.success() => {
            tracing::debug!(
                "[ecaa-validation] runcrate available: {}",
                String::from_utf8_lossy(&o.stdout).trim()
            );
            let started = Instant::now();
            let mut child = match Command::new("runcrate")
                .arg("validate")
                .arg(pkg_root)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    return ExternalCheckOutcome::Error {
                        reason: format!("runcrate spawn failed: {e}"),
                    };
                }
            };
            let out = loop {
                match child.try_wait() {
                    Ok(Some(_)) => match child.wait_with_output() {
                        Ok(o) => break o,
                        Err(e) => {
                            return ExternalCheckOutcome::Error {
                                reason: format!("wait_with_output failed: {e}"),
                            };
                        }
                    },
                    Ok(None) => {
                        if started.elapsed() >= timeout {
                            let _ = child.kill();
                            tracing::warn!(
                                timeout_secs = timeout.as_secs(),
                                "[ecaa-validation] runcrate timed out; killed"
                            );
                            return ExternalCheckOutcome::Error {
                                reason: format!("timeout after {}s", timeout.as_secs()),
                            };
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    Err(e) => {
                        return ExternalCheckOutcome::Error {
                            reason: format!("try_wait failed: {e}"),
                        };
                    }
                }
            };
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            if out.status.success() {
                tracing::info!("[ecaa-validation] runcrate PASS");
                ExternalCheckOutcome::Pass {
                    details: stdout.trim().to_string(),
                }
            } else {
                let details = format!("{} {}", stdout.trim(), stderr.trim());
                tracing::warn!(details = %details, "[ecaa-validation] runcrate FAIL");
                ExternalCheckOutcome::Fail { details }
            }
        }
        _ => {
            let reason = "runcrate not on PATH (pip install runcrate)".to_string();
            tracing::debug!(reason = %reason, "[ecaa-validation] runcrate unavailable");
            ExternalCheckOutcome::Unavailable { reason }
        }
    }
}

/// Run the full emit-time validation suite and return the aggregated
/// summary. Returns `Err` only when `SWFC_VALIDATION_BLOCK_ON_FAIL=1`
/// AND at least one schema failure was recorded. Otherwise warn-only.
pub fn validate_emitted_package(pkg_root: &Path) -> Result<ValidationSummary> {
    let start = Instant::now();
    let mode = read_mode();
    tracing::info!(
        "[ecaa-validation] begin pkg={} mode={:?}",
        pkg_root.display(),
        mode
    );

    if matches!(mode, ValidationMode::Disabled) {
        tracing::info!("[ecaa-validation] disabled via SWFC_VALIDATE_ON_EMIT");
        return Ok(ValidationSummary {
            schema_version: "0.1".to_string(),
            mode,
            schema_validation: SchemaValidationResults {
                passed: 0,
                failed: Vec::new(),
                skipped_pending_harness: 0,
            },
            external_validation: None,
            duration_ms: start.elapsed().as_millis(),
        });
    }

    let schema_validation = validate_schemas_pure_rust(pkg_root);
    let schema_failed = !schema_validation.failed.is_empty();

    let external_validation = match mode {
        ValidationMode::Disabled | ValidationMode::SchemaOnly => None,
        ValidationMode::Full => {
            let timeout = read_external_timeout();
            let scripts_dir = spec_scripts_dir();
            let (shacl, owl) = match scripts_dir.as_ref() {
                Some(dir) => (
                    run_external_check(
                        "shacl_projection",
                        "project_package.py",
                        &[pkg_root.to_str().unwrap_or(".")],
                        dir,
                        timeout,
                    ),
                    run_external_check("owl_consistency", "owl_consistency.py", &[], dir, timeout),
                ),
                None => {
                    let reason =
                        "scripts/spec-check/ not found (set SWFC_SPEC_SCRIPTS_DIR)".to_string();
                    (
                        ExternalCheckOutcome::Unavailable {
                            reason: reason.clone(),
                        },
                        ExternalCheckOutcome::Unavailable { reason },
                    )
                }
            };
            let runcrate = run_runcrate_validate(pkg_root, timeout);
            Some(ExternalValidationResults {
                shacl_projection: shacl,
                owl_consistency: owl,
                runcrate_validate: runcrate,
            })
        }
    };

    let summary = ValidationSummary {
        schema_version: "0.1".to_string(),
        mode,
        schema_validation,
        external_validation,
        duration_ms: start.elapsed().as_millis(),
    };
    tracing::info!(
        "[ecaa-validation] complete pkg={} duration_ms={} mode={:?}",
        pkg_root.display(),
        summary.duration_ms,
        summary.mode
    );

    if schema_failed && read_block_on_fail() {
        let n_failed = summary.schema_validation.failed.len();
        tracing::error!(
            "[ecaa-validation] BLOCKING emit: {} schema failures + SWFC_VALIDATION_BLOCK_ON_FAIL=1",
            n_failed
        );
        return Err(anyhow!(
            "ECAA emit-time validation blocked: {} schema failure(s) and SWFC_VALIDATION_BLOCK_ON_FAIL=1",
            n_failed
        ));
    }

    Ok(summary)
}

/// Serialize the summary to `runtime/validation-summary.json` (pretty,
/// trailing newline). Warn-only on I/O or serialization failure.
pub fn write_validation_summary(pkg_root: &Path, summary: &ValidationSummary) {
    let path = pkg_root.join("runtime").join("validation-summary.json");
    match serde_json::to_string_pretty(summary) {
        Ok(mut buf) => {
            buf.push('\n');
            if let Err(e) = std::fs::write(&path, buf) {
                tracing::warn!(
                    "[ecaa-validation] validation-summary.json write failed: {} (continuing emit)",
                    e
                );
            } else {
                tracing::debug!(
                    "[ecaa-validation] wrote validation-summary.json → {}",
                    path.display()
                );
            }
        }
        Err(e) => tracing::warn!(
            "[ecaa-validation] validation-summary serialization failed: {} (continuing emit)",
            e
        ),
    }
}
