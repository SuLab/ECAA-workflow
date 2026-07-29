//! Bounded, method-neutral, default-OFF autonomous recovery on a
//! required validation-contract block.
//!
//! # Problem
//!
//! When a `required` validation-contract assertion fails (e.g.
//! `variant_calling.het_tail_band_nonempty` — the operator-authored
//! design requires at least one called variant in the low-AF
//! heteroplasmy band, but the agent's own `result.json` recomputes
//! zero), [`crate::enforce_validation_contract`] re-blocks the parent
//! compute task. In the unattended (headless) eval path there is no SME
//! at the keyboard, so the harness idles to the iteration cap writing
//! `waiting_for_sme` and the task is dispatched exactly once with no
//! recovery — the result is a stranded block, not a corrected run.
//!
//! # Recovery (gated OFF by default)
//!
//! When `ECAA_HARNESS_VALIDATION_RECOVERY` is truthy, the harness may
//! re-dispatch the SAME agent a bounded number of times
//! ([`DEFAULT_MAX_VALIDATION_RECOVERY_ATTEMPTS`], hard-capped at
//! [`MAX_VALIDATION_RECOVERY_ATTEMPTS_CEILING`] = 2) AFTER surfacing the
//! failed assertion as a *domain-correctness signal*: the operator-
//! authored reference bound, recomputed against the agent's OWN
//! `result.json` numbers, with a plain-language statement of what is
//! biologically off.
//!
//! # Method neutrality (load-bearing)
//!
//! The signal reports WHAT is off, never HOW to fix it. It NEVER names a
//! tool, flag, aligner, caller, normalization, statistical test, or a
//! threshold *value to set*. It restates the assertion's own
//! operator-authored bound and the agent's own recomputed number, and
//! says "revisit". The agent chooses the method. This mirrors the
//! method-neutrality contract that the conversation-side
//! `remediation_proposer` and `prompt_role.txt` enforce.
//!
//! # Production / SME safety
//!
//! Default OFF preserves the human checkpoint: with the flag unset the
//! task stays Blocked and the SME-driven unblock path is unchanged. The
//! flag is intended for the unattended operator-run eval arm, whose
//! fairness disclosure (the bare arm has no retry loop) is recorded in
//! the per-task signal file so the scorecard meta can read it.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Env var that turns the autonomous recovery path ON. Default OFF.
pub const ENV_VALIDATION_RECOVERY: &str = "ECAA_HARNESS_VALIDATION_RECOVERY";

/// Env var that turns the advisory / warn-only contract path ON. Default
/// OFF. When ON, a failed `required` assertion is recorded as a
/// non-blocking diagnostic instead of re-blocking the task — see
/// [`advisory_enabled`].
pub const ENV_CONTRACT_ADVISORY: &str = "ECAA_HARNESS_CONTRACT_ADVISORY";

/// Env var that overrides the per-task recovery attempt budget. Clamped
/// to `[0, MAX_VALIDATION_RECOVERY_ATTEMPTS_CEILING]`.
pub const ENV_VALIDATION_RECOVERY_MAX: &str = "ECAA_HARNESS_VALIDATION_RECOVERY_MAX";

/// Default bounded number of autonomous re-dispatches per task on a
/// required-assertion block when recovery is enabled.
pub const DEFAULT_MAX_VALIDATION_RECOVERY_ATTEMPTS: u32 = 2;

/// Hard ceiling on the recovery budget. The spec bounds N ≤ 2; an
/// operator override cannot raise it past this. Keeps an unattended
/// arm from quietly turning into an unbounded retry loop (the bare arm
/// has no retry loop at all, so a large value would also skew fairness).
pub const MAX_VALIDATION_RECOVERY_ATTEMPTS_CEILING: u32 = 2;

/// Schema version of the on-disk signal file. Bumped if the shape ever
/// changes so a stale reader fails closed rather than misparsing.
pub const SIGNAL_SCHEMA_VERSION: u32 = 1;

/// Whether the autonomous validation-recovery path is enabled. Default
/// OFF — only the canonical truthy table (`1` / `true` / `yes` / `on` /
/// `t` / `y`, case-insensitive, matching `core::config`'s `parse_bool` so
/// the C7 catalog field and this live gate agree) enables it, so a typo
/// never silently enables a retry loop in the production / SME path.
pub fn recovery_enabled() -> bool {
    matches!(
        std::env::var(ENV_VALIDATION_RECOVERY)
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on") | Some("t") | Some("y")
    )
}

/// Whether the advisory / warn-only contract path is enabled. Default
/// OFF — only the canonical truthy table (matching `recovery_enabled` and
/// `core::config`'s `parse_bool`) enables it, so a typo never silently
/// turns a production / SME block into a non-blocking diagnostic.
///
/// When ON, ALL domain-correctness gates become non-blocking diagnostics:
/// (1) a failed `required` validation-CONTRACT assertion and (2) a failed
/// Phase-13 post-completion VALIDATOR are appended to the per-package
/// `runtime/validation-warnings.jsonl` sidecar instead of re-blocking the
/// task; (3) the server-side `claim_coverage` recall-gap gate (read there
/// via the typed `Config.harness_contract_advisory`, not this fn) suppresses
/// its Blocked transition while still persisting the gap into the signed
/// verdict sink + audit-proof report. Advisory mode takes PRECEDENCE over
/// [`recovery_enabled`] (no block, no re-dispatch).
pub fn advisory_enabled() -> bool {
    matches!(
        std::env::var(ENV_CONTRACT_ADVISORY)
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on") | Some("t") | Some("y")
    )
}

/// Resolve the per-task recovery attempt budget, clamped to
/// `[0, MAX_VALIDATION_RECOVERY_ATTEMPTS_CEILING]`. Unset / unparseable
/// falls back to [`DEFAULT_MAX_VALIDATION_RECOVERY_ATTEMPTS`].
pub fn max_recovery_attempts() -> u32 {
    let raw = std::env::var(ENV_VALIDATION_RECOVERY_MAX)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(DEFAULT_MAX_VALIDATION_RECOVERY_ATTEMPTS);
    raw.min(MAX_VALIDATION_RECOVERY_ATTEMPTS_CEILING)
}

/// One failed-assertion entry in the domain-correctness signal. Carries
/// ONLY the assertion id and a recomputed-bound-vs-agent-numbers
/// statement — no method, tool, or threshold-to-set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedAssertionSignal {
    /// The contract assertion id (e.g. `variant_calling.het_tail_band_nonempty`).
    pub assertion_id: String,
    /// Human-readable, method-neutral statement of what is biologically
    /// off: the design's required bound vs the number recomputed from
    /// the agent's own result.json.
    pub statement: String,
}

/// The on-disk domain-correctness signal written into the task's
/// next-run inputs so the re-dispatched agent reads what is off. Also
/// the DURABLE recovery budget tracker (the server auto-relaunches the
/// harness between dispatches, so an in-memory counter would reset).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainCorrectnessSignal {
    /// Signal-file schema version.
    pub schema_version: u32,
    /// The compute task this signal is for.
    pub task_id: String,
    /// The required assertions that failed, with their recomputed-bound
    /// statements. Deterministic order (the contract's authored order,
    /// deduped by the caller).
    pub failed_assertions: Vec<FailedAssertionSignal>,
    /// How many autonomous recovery re-dispatches have already been
    /// spent on this task. Incremented once per re-dispatch.
    pub recovery_attempts_consumed: u32,
    /// The budget ceiling in force when this signal was written
    /// (fairness disclosure for the scorecard meta).
    pub recovery_attempts_budget: u32,
    /// Constant disclosure: this task is in the autonomous-recovery arm,
    /// which has a bounded agent retry loop the bare arm does not. The
    /// scorecard meta reads this to keep arm comparisons honest.
    pub autonomous_recovery: bool,
}

/// On-disk location of the per-task domain-correctness signal. Lives
/// under `runtime/inputs/<task_id>/` alongside `overrides.json`, the
/// existing per-task next-run input channel.
pub fn signal_path(package: &Path, task_id: &str) -> PathBuf {
    package
        .join("runtime")
        .join("inputs")
        .join(task_id)
        .join("domain-correctness-signal.json")
}

/// Read the existing signal for a task. `Ok(None)` when absent (the
/// common case — no prior recovery). `Err` only on a present-but-broken
/// file, so the caller can fail closed (do not recover blindly).
pub fn read_signal(
    package: &Path,
    task_id: &str,
) -> anyhow::Result<Option<DomainCorrectnessSignal>> {
    use anyhow::Context as _;
    let path = signal_path(package, task_id);
    if !path.exists() {
        return Ok(None);
    }
    let raw = crate::ecaa_io::read_capped(&path, crate::ecaa_io::resolve_max_bytes())
        .with_context(|| format!("reading domain-correctness signal {}", path.display()))?;
    let parsed: DomainCorrectnessSignal = serde_json::from_str(&raw)
        .with_context(|| format!("parsing domain-correctness signal {}", path.display()))?;
    Ok(Some(parsed))
}

/// Write the signal atomically (`.tmp` + rename), creating the parent
/// directory lazily.
pub fn write_signal(
    package: &Path,
    task_id: &str,
    signal: &DomainCorrectnessSignal,
) -> anyhow::Result<()> {
    use anyhow::Context as _;
    let path = signal_path(package, task_id);
    let raw =
        serde_json::to_string_pretty(signal).context("serialising domain-correctness signal")?;
    ecaa_workflow_core::fs_helpers::atomic_write_bytes_sync(&path, raw.as_bytes()).with_context(
        || {
            format!(
                "atomic write domain-correctness signal at {}",
                path.display()
            )
        },
    )?;
    Ok(())
}

/// One advisory-warning record for a failed `required` validation-contract
/// assertion under the warn-only path. Appended to the per-package
/// `runtime/validation-warnings.jsonl` sidecar — one JSON line per
/// failure. Carries NO timestamp so the sidecar stays byte-deterministic
/// for a given DAG state.
///
/// Internal harness-crate artifact type: it never crosses the ts-rs /
/// RO-Crate / HTTP boundary, so the `#[non_exhaustive]` SemVer convention
/// for wire-facing types does not apply.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AdvisoryWarning {
    /// The compute task whose required assertion failed.
    pub task_id: String,
    /// The contract assertion id (e.g. `variant_calling.het_tail_band_nonempty`).
    pub assertion_id: String,
    /// Assertion severity. Always `required` today (only required
    /// assertions are evaluated), but recorded so the scorecard can
    /// distinguish if `recommended` ever joins the evaluated set.
    pub severity: String,
    /// The SAME reason text the block path computes — the task-level
    /// "required assertion(s) unsatisfied: ..." string the SME would have
    /// seen had the task been blocked.
    pub reason: String,
}

/// On-disk location of the per-package advisory-warnings sidecar. Lives at
/// `runtime/validation-warnings.jsonl` — a runtime artifact (like
/// `result.json`), excluded from the byte-repro baseline but written in a
/// deterministic order so its contents are stable for a given DAG state.
pub fn warnings_path(package: &Path) -> PathBuf {
    package.join("runtime").join("validation-warnings.jsonl")
}

/// Merge `new_warnings` into the package's advisory-warnings sidecar and
/// rewrite it atomically. The enforcer runs every iteration, so the sidecar
/// is REWRITTEN (not blindly appended) as the deduped union of any existing
/// records plus the new ones, in a deterministic (sorted, then deduped)
/// order — a failed assertion produces the same single line no matter how
/// many enforcement passes observe it. Unreadable existing content is
/// treated as empty (fail-open: a corrupt sidecar must not brick the loop).
pub fn append_warnings(package: &Path, new_warnings: &[AdvisoryWarning]) -> anyhow::Result<()> {
    use anyhow::Context as _;
    if new_warnings.is_empty() {
        return Ok(());
    }
    let path = warnings_path(package);
    let mut all: Vec<AdvisoryWarning> = Vec::new();
    if path.exists() {
        if let Ok(raw) = crate::ecaa_io::read_capped(&path, crate::ecaa_io::resolve_max_bytes()) {
            for line in raw.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Ok(w) = serde_json::from_str::<AdvisoryWarning>(line) {
                    all.push(w);
                }
            }
        }
    }
    all.extend(new_warnings.iter().cloned());
    all.sort();
    all.dedup();
    let mut body = String::new();
    for w in &all {
        let line = serde_json::to_string(w).context("serialising advisory warning")?;
        body.push_str(&line);
        body.push('\n');
    }
    ecaa_workflow_core::fs_helpers::atomic_write_bytes_sync(&path, body.as_bytes())
        .with_context(|| format!("atomic write advisory warnings at {}", path.display()))?;
    Ok(())
}

/// Build the advisory warnings for a Phase-13 post-completion validator
/// bundle that reported failures. Returns one [`AdvisoryWarning`] per FAILED
/// validator row (its `obligation_id` is the assertion id); errored,
/// unimplemented, and passed rows are skipped (only a hard `Failed` outcome is
/// a domain-correctness violation). `reason` is the SAME `[validation_failed]`
/// task-level string the strict block path computes, recorded verbatim so the
/// sidecar carries exactly what the SME would have seen on a block.
///
/// Pure + deterministic (rows are visited in summary order); the caller gates
/// on [`advisory_enabled`] and persists the result via [`append_warnings`].
pub fn phase13_advisory_warnings(
    task_id: &str,
    summary: &crate::validators::ValidationReportSummary,
    reason: &str,
) -> Vec<AdvisoryWarning> {
    summary
        .rows
        .iter()
        .filter(|r| {
            matches!(
                r.outcome,
                crate::validators::ValidatorOutcome::Failed { .. }
            )
        })
        .map(|r| AdvisoryWarning {
            task_id: task_id.to_string(),
            assertion_id: r.obligation_id.clone(),
            severity: "required".to_string(),
            reason: reason.to_string(),
        })
        .collect()
}

/// Read a JSON value at `pointer` (RFC-6901) from `path` as `f64`.
/// `None` on missing/unparseable file, an unresolved pointer, or a
/// non-numeric value. Pessimistic by construction (None at the call
/// site renders as "not present"). Capped read, mirroring the binary's
/// `read_json_pointer_f64`.
fn pointer_f64(path: &Path, pointer: &str) -> Option<f64> {
    let bytes =
        crate::ecaa_io::read_bytes_capped(path, crate::ecaa_io::resolve_max_bytes()).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.pointer(pointer).and_then(|x| x.as_f64())
}

/// Read a JSON array of numbers at `pointer` into a `Vec<f64>`. `None`
/// when the file/pointer is missing or the value is not an array;
/// non-numeric elements are skipped. Capped read.
fn pointer_f64_array(path: &Path, pointer: &str) -> Option<Vec<f64>> {
    let bytes =
        crate::ecaa_io::read_bytes_capped(path, crate::ecaa_io::resolve_max_bytes()).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let arr = v.pointer(pointer)?.as_array()?;
    Some(arr.iter().filter_map(|x| x.as_f64()).collect())
}

/// Format an `f64` that is conceptually a count / bound for the neutral
/// statement: integers render without a trailing `.0`, fractions keep up
/// to 4 significant decimals. Deterministic.
fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        // Trim trailing zeros from a fixed-precision render.
        let s = format!("{v:.4}");
        let trimmed = s.trim_end_matches('0').trim_end_matches('.');
        trimmed.to_string()
    }
}

/// Human phrasing for a comparison operator, stated as the DESIGN'S
/// REQUIREMENT (what the result must satisfy), never as an instruction
/// to set a threshold. e.g. "at least", "at most".
fn op_requirement_phrase(op: &str) -> &'static str {
    match op {
        "gte" => "at least",
        "gt" => "more than",
        "lte" => "at most",
        "lt" => "fewer than",
        "eq" => "exactly",
        _ => "consistent with",
    }
}

/// Build the method-neutral recomputed-bound-vs-agent-numbers statement
/// for a single failed assertion, reading the agent's OWN numbers back
/// out of `result.json` (under `pkg_dir`). The statement names only the
/// quantity the assertion measures, the design's operator-authored
/// bound, and the value recomputed from the agent's result — never a
/// tool, flag, or a value to set.
///
/// `upstream` maps an upstream task_id to its output dir so a
/// cross-stage comparison can read the producer's number (mirrors
/// `run_assertion`'s signature). Deterministic.
pub fn build_statement(
    pkg_dir: &Path,
    assertion: &serde_json::Value,
    upstream: &BTreeMap<String, PathBuf>,
) -> String {
    let id = assertion
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("(unnamed assertion)");
    let atype = assertion
        .get("assertion_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let target = assertion.get("target").and_then(|v| v.as_str());
    let check = assertion.get("check");

    // Resolve the agent's own result file for this assertion.
    let resolve = |t: &str| -> PathBuf { pkg_dir.join(t.trim_start_matches('/')) };

    match atype {
        "numeric_threshold" => {
            let ptr = check
                .and_then(|c| c.get("json_pointer"))
                .and_then(|v| v.as_str());
            let op = check.and_then(|c| c.get("op")).and_then(|v| v.as_str());
            let bound = check.and_then(|c| c.get("value")).and_then(|v| v.as_f64());
            let observed = match (target, ptr) {
                (Some(t), Some(p)) => pointer_f64(&resolve(t), p),
                _ => None,
            };
            match (op, bound) {
                (Some(op), Some(bound)) => {
                    let req = op_requirement_phrase(op);
                    let obs = observed
                        .map(fmt_num)
                        .unwrap_or_else(|| "not present in your result.json".to_string());
                    format!(
                        "{id}: this design requires {field} {req} {bound}, but your result.json recomputes {obs}. \
                         The biological expectation behind this check is not met — revisit your analysis so the recomputed value satisfies the design's bound. \
                         (How you achieve that is your choice; no method, tool, or threshold value is prescribed.)",
                        field = ptr.unwrap_or("the measured quantity"),
                        bound = fmt_num(bound),
                    )
                }
                _ => generic_statement(id),
            }
        }
        "numeric_distribution" => {
            let ptr = check
                .and_then(|c| c.get("json_pointer"))
                .and_then(|v| v.as_str());
            let stat = check.and_then(|c| c.get("stat")).and_then(|v| v.as_str());
            let op = check.and_then(|c| c.get("op")).and_then(|v| v.as_str());
            let bound = check.and_then(|c| c.get("value")).and_then(|v| v.as_f64());
            let observed = match (target, ptr, stat) {
                (Some(t), Some(p), Some(s)) => pointer_f64_array(&resolve(t), p).and_then(|vals| {
                    if vals.is_empty() {
                        None
                    } else {
                        let dist =
                            ecaa_workflow_core::statistical_helpers::compute_distribution_stats(
                                &vals,
                            );
                        match s {
                            "mean" => Some(dist.mean),
                            "stdev" => Some(dist.stdev),
                            "skewness" => Some(dist.skewness),
                            "kurtosis" => Some(dist.kurtosis),
                            "p5" => Some(dist.p5),
                            "p50" => Some(dist.p50),
                            "p95" => Some(dist.p95),
                            _ => None,
                        }
                    }
                }),
                _ => None,
            };
            match (stat, op, bound) {
                (Some(stat), Some(op), Some(bound)) => {
                    let req = op_requirement_phrase(op);
                    let obs = observed.map(fmt_num).unwrap_or_else(|| {
                        "could not be recomputed from your result.json".to_string()
                    });
                    format!(
                        "{id}: this design requires the {stat} of {field} to be {req} {bound}, but your result.json recomputes {obs}. \
                         The distribution your analysis produced is outside the design's expectation — revisit it. \
                         (How you achieve that is your choice; no method, tool, or threshold value is prescribed.)",
                        field = ptr.unwrap_or("the measured distribution"),
                        bound = fmt_num(bound),
                    )
                }
                _ => generic_statement(id),
            }
        }
        "reference_range_outlier" => {
            let ptr = check
                .and_then(|c| c.get("json_pointer"))
                .and_then(|v| v.as_str());
            let rmin = check
                .and_then(|c| c.get("reference_min"))
                .and_then(|v| v.as_f64());
            let rmax = check
                .and_then(|c| c.get("reference_max"))
                .and_then(|v| v.as_f64());
            let observed = match (target, ptr) {
                (Some(t), Some(p)) => pointer_f64_array(&resolve(t), p),
                _ => None,
            };
            let obs_desc = match observed {
                Some(vals) if !vals.is_empty() => {
                    let lo = vals.iter().cloned().fold(f64::INFINITY, f64::min);
                    let hi = vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                    format!("values spanning [{}, {}]", fmt_num(lo), fmt_num(hi))
                }
                _ => "values that could not be recomputed from your result.json".to_string(),
            };
            match (rmin, rmax) {
                (Some(rmin), Some(rmax)) => format!(
                    "{id}: this design expects {field} to fall within the reference range [{lo}, {hi}], but your result.json recomputes {obs_desc}. \
                     One or more values are outside the biologically plausible range — revisit your analysis. \
                     (How you achieve that is your choice; no method, tool, or threshold value is prescribed.)",
                    field = ptr.unwrap_or("the measured quantity"),
                    lo = fmt_num(rmin),
                    hi = fmt_num(rmax),
                ),
                _ => generic_statement(id),
            }
        }
        "cross_stage_output_comparison" => {
            let this_ptr = check
                .and_then(|c| c.get("this_pointer"))
                .and_then(|v| v.as_str());
            let up_task = check
                .and_then(|c| c.get("upstream_task"))
                .and_then(|v| v.as_str());
            let up_file = check
                .and_then(|c| c.get("upstream_file"))
                .and_then(|v| v.as_str())
                .unwrap_or("result.json");
            let up_ptr = check
                .and_then(|c| c.get("upstream_pointer"))
                .and_then(|v| v.as_str());
            let op = check.and_then(|c| c.get("op")).and_then(|v| v.as_str());
            let this_val = match (target, this_ptr) {
                (Some(t), Some(p)) => pointer_f64(&resolve(t), p),
                _ => None,
            };
            let up_val = match (up_task, up_ptr) {
                (Some(ut), Some(up)) => upstream
                    .get(ut)
                    .map(|dir| dir.join(up_file))
                    .and_then(|p| pointer_f64(&p, up)),
                _ => None,
            };
            match op {
                Some(op) => {
                    let req = op_requirement_phrase(op);
                    let this_s = this_val
                        .map(fmt_num)
                        .unwrap_or_else(|| "missing".to_string());
                    let up_s = up_val.map(fmt_num).unwrap_or_else(|| "missing".to_string());
                    format!(
                        "{id}: this design requires this stage's {this_field} ({this_s}) to be {req} the upstream {up_task}'s {up_field} ({up_s}), but your result.json values do not satisfy that cross-stage invariant. \
                         Revisit your analysis so the stages stay consistent. \
                         (How you achieve that is your choice; no method, tool, or threshold value is prescribed.)",
                        this_field = this_ptr.unwrap_or("measured value"),
                        up_task = up_task.unwrap_or("upstream stage"),
                        up_field = up_ptr.unwrap_or("measured value"),
                    )
                }
                None => generic_statement(id),
            }
        }
        _ => generic_statement(id),
    }
}

/// Fallback neutral statement for assertion shapes that carry no numeric
/// bound to recompute (presence / control / substring checks). Still
/// names only the assertion, never a method.
fn generic_statement(id: &str) -> String {
    format!(
        "{id}: this required domain-correctness check is not satisfied by your result.json. \
         The biological expectation behind it is not met — revisit your analysis until the check's recomputed value satisfies the design. \
         (How you achieve that is your choice; no method, tool, or threshold value is prescribed.)"
    )
}

/// Decision returned by [`plan_recovery`]: whether to re-dispatch the
/// blocked task this iteration, and the signal to persist if so.
///
/// Internal harness-crate planner type: it never crosses the ts-rs /
/// RO-Crate / HTTP boundary and is matched exhaustively only within this
/// crate, so the `#[non_exhaustive]` SemVer convention (for wire-facing
/// enums) does not apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryDecision {
    /// Recovery is disabled, the budget is exhausted, or no required
    /// assertion failed — leave the task Blocked (the SME / waiting_for_sme
    /// path is unchanged).
    LeaveBlocked,
    /// Re-dispatch: flip the task back to a dispatchable state and persist
    /// `signal`. `attempt_number` is 1-based (the attempt about to run).
    Redispatch {
        /// The signal to write into the task's next-run inputs.
        signal: DomainCorrectnessSignal,
        /// 1-based index of the recovery attempt about to be dispatched.
        attempt_number: u32,
    },
}

/// Pure recovery planner. Given the prior on-disk signal (the durable
/// budget across harness relaunches), the env-resolved budget, the
/// failed-assertion signals for this block, and whether recovery is
/// enabled, decide whether to re-dispatch.
///
/// Pure + deterministic so the bound is unit-testable without touching
/// the filesystem or the env. The caller resolves `enabled` /
/// `budget` / `prior` from the environment + disk and persists the
/// returned signal.
pub fn plan_recovery(
    task_id: &str,
    enabled: bool,
    budget: u32,
    prior: Option<&DomainCorrectnessSignal>,
    failed: Vec<FailedAssertionSignal>,
) -> RecoveryDecision {
    if !enabled || budget == 0 || failed.is_empty() {
        return RecoveryDecision::LeaveBlocked;
    }
    let already = prior.map(|p| p.recovery_attempts_consumed).unwrap_or(0);
    if already >= budget {
        // Budget exhausted across (possibly multiple) harness relaunches —
        // the task stays Blocked for the SME, exactly as without recovery.
        return RecoveryDecision::LeaveBlocked;
    }
    let consumed = already.saturating_add(1);
    RecoveryDecision::Redispatch {
        signal: DomainCorrectnessSignal {
            schema_version: SIGNAL_SCHEMA_VERSION,
            task_id: task_id.to_string(),
            failed_assertions: failed,
            recovery_attempts_consumed: consumed,
            recovery_attempts_budget: budget,
            autonomous_recovery: true,
        },
        attempt_number: consumed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn met_floor() -> &'static str {
        // A phrase the neutral statements must NEVER contain — a method,
        // tool, flag, or a threshold value to set. Used by the
        // neutrality assertions below.
        "set the threshold to"
    }

    /// The neutrality guard: no statement may name a tool/method/flag or
    /// instruct a threshold value. Scans for a denylist of method tokens.
    fn assert_method_neutral(s: &str) {
        let lower = s.to_ascii_lowercase();
        for token in [
            "bwa",
            "bowtie",
            "minimap",
            "gatk",
            "lofreq",
            "mutect",
            "bcftools",
            "samtools",
            "freebayes",
            "deseq",
            "edger",
            "limma",
            "salmon",
            "star",
            "hisat",
            "deseq2",
            "--",
            "set the threshold",
            "set --",
            "use the tool",
            "aligner",
            "caller ",
            "normalization method",
            "t-test",
            "wilcoxon",
        ] {
            assert!(
                !lower.contains(token),
                "neutral signal leaked a method/tool/flag token {token:?}: {s}"
            );
        }
        assert!(
            !s.contains(met_floor()),
            "leaked threshold-set instruction: {s}"
        );
    }

    #[test]
    fn recovery_enable_default_off_and_parses_truthy_only() {
        // Single test owns the ENV_VALIDATION_RECOVERY var so two tests
        // can't race on the shared process env (cargo test is threaded;
        // the project's nextest runner is process-isolated, but this is
        // robust under either).
        std::env::remove_var(ENV_VALIDATION_RECOVERY);
        assert!(
            !recovery_enabled(),
            "must default OFF with the var unset (preserves the SME checkpoint)"
        );
        for (v, want) in [
            ("1", true),
            ("true", true),
            ("YES", true),
            ("on", true),
            ("0", false),
            ("false", false),
            ("maybe", false),
            ("", false),
        ] {
            std::env::set_var(ENV_VALIDATION_RECOVERY, v);
            assert_eq!(recovery_enabled(), want, "value {v:?}");
        }
        std::env::remove_var(ENV_VALIDATION_RECOVERY);
    }

    #[test]
    fn budget_clamped_to_ceiling() {
        std::env::set_var(ENV_VALIDATION_RECOVERY_MAX, "99");
        assert_eq!(
            max_recovery_attempts(),
            MAX_VALIDATION_RECOVERY_ATTEMPTS_CEILING,
            "an operator override cannot exceed the hard ceiling"
        );
        std::env::set_var(ENV_VALIDATION_RECOVERY_MAX, "1");
        assert_eq!(max_recovery_attempts(), 1);
        std::env::remove_var(ENV_VALIDATION_RECOVERY_MAX);
        assert_eq!(
            max_recovery_attempts(),
            DEFAULT_MAX_VALIDATION_RECOVERY_ATTEMPTS
        );
    }

    #[test]
    fn ceiling_is_two() {
        // The spec bounds N ≤ 2; pin it so a future bump is a conscious
        // change, not an accident.
        assert_eq!(MAX_VALIDATION_RECOVERY_ATTEMPTS_CEILING, 2);
        assert!(
            DEFAULT_MAX_VALIDATION_RECOVERY_ATTEMPTS <= MAX_VALIDATION_RECOVERY_ATTEMPTS_CEILING
        );
    }

    #[test]
    fn plan_recovery_respects_disabled() {
        let failed = vec![FailedAssertionSignal {
            assertion_id: "variant_calling.het_tail_band_nonempty".into(),
            statement: "x".into(),
        }];
        assert_eq!(
            plan_recovery("variant_calling", false, 2, None, failed.clone()),
            RecoveryDecision::LeaveBlocked,
            "disabled must never re-dispatch"
        );
        assert_eq!(
            plan_recovery("variant_calling", true, 0, None, failed),
            RecoveryDecision::LeaveBlocked,
            "zero budget must never re-dispatch"
        );
    }

    #[test]
    fn plan_recovery_no_failed_assertions_leaves_blocked() {
        assert_eq!(
            plan_recovery("variant_calling", true, 2, None, vec![]),
            RecoveryDecision::LeaveBlocked
        );
    }

    #[test]
    fn plan_recovery_bounds_at_two_attempts() {
        let failed = || {
            vec![FailedAssertionSignal {
                assertion_id: "variant_calling.het_tail_band_nonempty".into(),
                statement: "band has 0 calls, design requires >=1".into(),
            }]
        };
        // First attempt: prior None -> consumed 1.
        let d1 = plan_recovery("variant_calling", true, 2, None, failed());
        let s1 = match d1 {
            RecoveryDecision::Redispatch {
                signal,
                attempt_number,
            } => {
                assert_eq!(attempt_number, 1);
                assert_eq!(signal.recovery_attempts_consumed, 1);
                assert_eq!(signal.recovery_attempts_budget, 2);
                assert!(signal.autonomous_recovery);
                signal
            }
            other => panic!("expected redispatch, got {other:?}"),
        };
        // Second attempt: prior = s1 (consumed 1) -> consumed 2.
        let d2 = plan_recovery("variant_calling", true, 2, Some(&s1), failed());
        let s2 = match d2 {
            RecoveryDecision::Redispatch {
                signal,
                attempt_number,
            } => {
                assert_eq!(attempt_number, 2);
                assert_eq!(signal.recovery_attempts_consumed, 2);
                signal
            }
            other => panic!("expected redispatch, got {other:?}"),
        };
        // Third attempt: budget exhausted (consumed 2 >= budget 2).
        assert_eq!(
            plan_recovery("variant_calling", true, 2, Some(&s2), failed()),
            RecoveryDecision::LeaveBlocked,
            "must stop after the bounded number of attempts"
        );
    }

    #[test]
    fn statement_numeric_threshold_is_neutral_and_recomputed() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/variant_calling")).unwrap();
        // Agent's own result: the low-AF band has 0 calls.
        std::fs::write(
            pkg.join("runtime/outputs/variant_calling/result.json"),
            r#"{"low_af_band_count": 0}"#,
        )
        .unwrap();
        let assertion = json!({
            "id": "variant_calling.het_tail_band_nonempty",
            "assertion_type": "numeric_threshold",
            "target": "runtime/outputs/variant_calling/result.json",
            "check": { "json_pointer": "/low_af_band_count", "op": "gte", "value": 1.0 }
        });
        let s = build_statement(pkg, &assertion, &BTreeMap::new());
        assert_method_neutral(&s);
        // Names the assertion, the design's bound, and the agent's own number.
        assert!(s.contains("variant_calling.het_tail_band_nonempty"), "{s}");
        assert!(
            s.contains("at least 1"),
            "must restate the design bound: {s}"
        );
        assert!(
            s.contains("recomputes 0"),
            "must restate the agent's own number: {s}"
        );
        assert!(s.contains("revisit"), "must say revisit, not how: {s}");
    }

    #[test]
    fn statement_missing_value_is_neutral() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/variant_calling")).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/variant_calling/result.json"),
            r#"{"unrelated": true}"#,
        )
        .unwrap();
        let assertion = json!({
            "id": "variant_calling.het_tail_band_nonempty",
            "assertion_type": "numeric_threshold",
            "target": "runtime/outputs/variant_calling/result.json",
            "check": { "json_pointer": "/low_af_band_count", "op": "gte", "value": 1.0 }
        });
        let s = build_statement(pkg, &assertion, &BTreeMap::new());
        assert_method_neutral(&s);
        assert!(s.contains("not present in your result.json"), "{s}");
    }

    #[test]
    fn statement_cross_stage_is_neutral() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/variant_filtering")).unwrap();
        std::fs::create_dir_all(pkg.join("runtime/outputs/variant_calling")).unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/variant_filtering/result.json"),
            r#"{"variant_count": 50}"#,
        )
        .unwrap();
        std::fs::write(
            pkg.join("runtime/outputs/variant_calling/result.json"),
            r#"{"variant_count": 30}"#,
        )
        .unwrap();
        let mut upstream = BTreeMap::new();
        upstream.insert(
            "variant_calling".to_string(),
            pkg.join("runtime/outputs/variant_calling"),
        );
        let assertion = json!({
            "id": "variant_filtering.filtered_le_called",
            "assertion_type": "cross_stage_output_comparison",
            "target": "runtime/outputs/variant_filtering/result.json",
            "check": {
                "this_pointer": "/variant_count",
                "upstream_task": "variant_calling",
                "upstream_file": "result.json",
                "upstream_pointer": "/variant_count",
                "op": "lte"
            }
        });
        let s = build_statement(pkg, &assertion, &upstream);
        assert_method_neutral(&s);
        assert!(s.contains("50"), "must restate this stage's number: {s}");
        assert!(s.contains("30"), "must restate the upstream number: {s}");
        assert!(
            s.contains("at most"),
            "must restate the design requirement: {s}"
        );
    }

    #[test]
    fn signal_roundtrips_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let sig = DomainCorrectnessSignal {
            schema_version: SIGNAL_SCHEMA_VERSION,
            task_id: "variant_calling".into(),
            failed_assertions: vec![FailedAssertionSignal {
                assertion_id: "variant_calling.het_tail_band_nonempty".into(),
                statement: "band has 0 calls".into(),
            }],
            recovery_attempts_consumed: 1,
            recovery_attempts_budget: 2,
            autonomous_recovery: true,
        };
        write_signal(pkg, "variant_calling", &sig).unwrap();
        let back = read_signal(pkg, "variant_calling").unwrap().unwrap();
        assert_eq!(back, sig);
        // Absent file -> Ok(None).
        assert!(read_signal(pkg, "nope").unwrap().is_none());
    }

    #[test]
    fn advisory_enable_default_off_and_parses_truthy_only() {
        // One test owns ENV_CONTRACT_ADVISORY so threaded cargo test can't
        // race on the shared process env.
        std::env::remove_var(ENV_CONTRACT_ADVISORY);
        assert!(
            !advisory_enabled(),
            "must default OFF with the var unset (preserves the SME checkpoint)"
        );
        for (v, want) in [
            ("1", true),
            ("true", true),
            ("YES", true),
            ("on", true),
            ("0", false),
            ("false", false),
            ("maybe", false),
            ("", false),
        ] {
            std::env::set_var(ENV_CONTRACT_ADVISORY, v);
            assert_eq!(advisory_enabled(), want, "value {v:?}");
        }
        std::env::remove_var(ENV_CONTRACT_ADVISORY);
    }

    #[test]
    fn append_warnings_is_deduped_sorted_and_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let w = |task: &str, assertion: &str| AdvisoryWarning {
            task_id: task.to_string(),
            assertion_id: assertion.to_string(),
            severity: "required".to_string(),
            reason: "required assertion(s) unsatisfied".to_string(),
        };
        // First pass writes two warnings.
        append_warnings(
            pkg,
            &[
                w("variant_calling", "variant_calling.het_tail_band_nonempty"),
                w("qc", "qc.manifest_present"),
            ],
        )
        .unwrap();
        // Second pass repeats one and adds a new one — the repeat must not
        // duplicate, and the file is rewritten in sorted order.
        append_warnings(
            pkg,
            &[
                w("variant_calling", "variant_calling.het_tail_band_nonempty"),
                w("annotation", "annotation.gene_symbols_present"),
            ],
        )
        .unwrap();
        let raw = std::fs::read_to_string(warnings_path(pkg)).unwrap();
        let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 3, "deduped union of distinct warnings: {raw}");
        // Sorted by (task_id, assertion_id): annotation < qc < variant_calling.
        let parsed: Vec<AdvisoryWarning> = lines
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(parsed[0].task_id, "annotation");
        assert_eq!(parsed[1].task_id, "qc");
        assert_eq!(parsed[2].task_id, "variant_calling");
        // Empty input is a no-op.
        let before = std::fs::read_to_string(warnings_path(pkg)).unwrap();
        append_warnings(pkg, &[]).unwrap();
        assert_eq!(std::fs::read_to_string(warnings_path(pkg)).unwrap(), before);
    }

    /// Build a Phase-13 summary with one failed + one passed + one errored
    /// row, so the advisory builder can prove it only records FAILED rows.
    fn phase13_summary_mixed() -> crate::validators::ValidationReportSummary {
        use crate::validators::{ValidatorOutcome, ValidatorRow};
        crate::validators::ValidationReportSummary {
            task_id: "variant_calling".into(),
            rows: vec![
                ValidatorRow {
                    obligation_id: "het_tail_band_nonempty".into(),
                    outcome: ValidatorOutcome::Failed {
                        message: "band has 0 calls".into(),
                    },
                },
                ValidatorRow {
                    obligation_id: "p_value_in_unit_interval".into(),
                    outcome: ValidatorOutcome::Passed,
                },
                ValidatorRow {
                    obligation_id: "manifest_present".into(),
                    outcome: ValidatorOutcome::Errored {
                        reason: "result.json missing".into(),
                    },
                },
            ],
        }
    }

    #[test]
    fn phase13_advisory_warnings_only_records_failed_rows() {
        let summary = phase13_summary_mixed();
        let reason = "[validation_failed] task=variant_calling task variant_calling: 1/3 passed, 1 failed, 1 errored, 0 unimplemented — Phase 13 validator(s) reported failures.";
        let warnings = phase13_advisory_warnings("variant_calling", &summary, reason);
        assert_eq!(warnings.len(), 1, "only the FAILED row becomes a warning");
        assert_eq!(warnings[0].task_id, "variant_calling");
        assert_eq!(warnings[0].assertion_id, "het_tail_band_nonempty");
        assert_eq!(warnings[0].severity, "required");
        assert_eq!(
            warnings[0].reason, reason,
            "the [validation_failed] task-level reason is recorded verbatim"
        );
    }

    #[test]
    fn phase13_advisory_path_records_to_sidecar_and_leaves_no_block() {
        // End-to-end of the advisory side of SITE 1 at the library boundary:
        // building the warnings from a failing summary and persisting them
        // produces exactly the sidecar a re-block path would NOT have written
        // (the task stays completed; no block state is involved here).
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let summary = phase13_summary_mixed();
        let reason = "[validation_failed] task=variant_calling band has 0 calls — Phase 13 validator(s) reported failures.";
        let warnings = phase13_advisory_warnings("variant_calling", &summary, reason);
        append_warnings(pkg, &warnings).unwrap();
        let raw = std::fs::read_to_string(warnings_path(pkg)).unwrap();
        let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 1, "one warning line for the single failed row");
        let parsed: AdvisoryWarning = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed.assertion_id, "het_tail_band_nonempty");
        assert_eq!(parsed.reason, reason);
    }

    #[test]
    fn phase13_advisory_off_means_no_warnings_recorded_regression() {
        // Regression mirror of the strict (advisory OFF) path: with the flag
        // unset the harness takes the block branch and NEVER calls
        // phase13_advisory_warnings / append_warnings, so the sidecar stays
        // absent. We assert the gate stays OFF by default and that an empty
        // warnings persist is a no-op (the strict path's observable footprint
        // on the warnings sidecar is nil).
        std::env::remove_var(ENV_CONTRACT_ADVISORY);
        assert!(
            !advisory_enabled(),
            "advisory must default OFF so the strict Phase-13 block path runs"
        );
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        // The strict path records nothing to the warnings sidecar.
        append_warnings(pkg, &[]).unwrap();
        assert!(
            !warnings_path(pkg).exists(),
            "strict path must leave the advisory sidecar absent"
        );
    }
}
