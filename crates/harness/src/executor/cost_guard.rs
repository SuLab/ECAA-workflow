//! Cost guard that estimates the spend of a planned run before
//! `AwsExecutor::provision` launches an instance. When the projected
//! total exceeds `SWFC_AWS_COST_CEILING_USD`, `provision` aborts
//! with a clear error pointing at the ceiling value.
//!
//! R-21 — alongside the per-provision ceiling check there's a
//! cumulative-spend tracker (`CumulativeSpend`) that persists the
//! running total to a per-package sidecar file. A run that piecewise
//! stays under the per-provision ceiling but spends $100+ over many
//! cycles (e.g. spot reclaim → reprovision loops) is caught by the
//! cumulative ceiling, default $100 via `SWFC_AWS_RUN_TOTAL_CEILING_USD`.

use super::aws::pricing::{on_demand_usd_per_hour, SPOT_DISCOUNT_FRACTION};
use std::env;
use std::path::{Path, PathBuf};

/// Source of the rate table. Real cloud pricing data drifts; we
/// include a minimal hand-curated table for the instance types the
/// sizing resolver currently emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PricingSource {
    /// Use the published on-demand rate for the instance type.
    OnDemand,
    /// Use the estimated Spot rate (on-demand × `SPOT_DISCOUNT_FRACTION`).
    Spot,
}

/// Per-instance-type hourly rate in USD. Values come from the versioned
/// `executor::aws::pricing` table (see that module for revision date and
/// refresh procedure). Spot is computed as on-demand × `SPOT_DISCOUNT_FRACTION`.
pub fn hourly_rate_usd(instance_type: &str, source: PricingSource) -> Option<f64> {
    let on_demand = on_demand_usd_per_hour(instance_type)?;
    match source {
        PricingSource::OnDemand => Some(on_demand),
        PricingSource::Spot => Some(on_demand * SPOT_DISCOUNT_FRACTION),
    }
}

/// Estimate the spend for a planned run given a sequence of task
/// (instance_type, expected_hours) pairs.
///
/// Returns `Err` when any instance type is unknown — the harness
/// should fail loud rather than silently cost-guard a run whose
/// pricing we can't verify.
pub fn estimate_run_cost_usd(
    tasks: &[(String, f64)],
    source: PricingSource,
) -> Result<f64, CostGuardError> {
    let mut total = 0.0;
    for (instance_type, hours) in tasks {
        let rate = hourly_rate_usd(instance_type, source)
            .ok_or_else(|| CostGuardError::UnknownInstanceType(instance_type.clone()))?;
        if *hours < 0.0 {
            return Err(CostGuardError::NegativeHours(instance_type.clone(), *hours));
        }
        total += rate * hours;
    }
    Ok(total)
}

/// Check the estimated spend against `SWFC_AWS_COST_CEILING_USD`.
///
/// The ceiling is REQUIRED for AWS provisioning. An unset /
/// unparseable / non-positive value MUST refuse to provision —
/// silently disabling the ceiling would close off the only guard
/// against runaway AWS spend if a sizing miss or misclassification
/// cascaded into a $1000+ instance type. So:
///
/// * Unset env var → `Err(CeilingUnset)`
/// * Unparseable / zero / negative value → `Err(CeilingUnset)`
/// * Positive parseable value → enforced as before
///
/// Callers that don't want a ceiling at all should pick
/// `BackendKind::NoEstimate` (the SLURM / local executors already use
/// it; see `check_ceiling_for_backend`). AWS provisioning has no such
/// opt-out path.
pub fn check_ceiling(estimated_usd: f64) -> Result<(), CostGuardError> {
    let ceiling_raw = match env::var(PER_PROVISION_CEILING_ENV) {
        Ok(v) => v,
        Err(_) => {
            return Err(CostGuardError::CeilingUnset {
                which_env: PER_PROVISION_CEILING_ENV,
            })
        }
    };
    let ceiling: f64 = match ceiling_raw.trim().parse() {
        Ok(v) if v > 0.0 => v,
        Ok(_) | Err(_) => {
            return Err(CostGuardError::CeilingUnset {
                which_env: PER_PROVISION_CEILING_ENV,
            })
        }
    };
    if estimated_usd > ceiling {
        return Err(CostGuardError::CeilingExceeded {
            estimated_usd,
            ceiling_usd: ceiling,
        });
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
/// Errors from cost-guard ceiling checks and cumulative spend operations.
pub enum CostGuardError {
    /// The requested instance type has no entry in the pricing table.
    UnknownInstanceType(String),
    /// A negative wall-clock hour value was supplied for an instance type.
    NegativeHours(String, f64),
    /// Estimated spend exceeds the per-provision ceiling.
    CeilingExceeded {
        /// Projected spend in USD for this provision.
        estimated_usd: f64,
        /// Ceiling value in USD from `SWFC_AWS_COST_CEILING_USD`.
        ceiling_usd: f64,
    },
    /// One of the cost-ceiling env vars (`SWFC_AWS_COST_CEILING_USD` for
    /// per-provision, `SWFC_AWS_RUN_TOTAL_CEILING_USD` for cumulative)
    /// is unset, empty, non-positive, or unparseable. AWS provisioning
    /// fails closed: the operator must set a positive USD value
    /// explicitly before any `aws ec2 run-instances` call can fire.
    /// `which_env` names the specific variable that triggered the
    /// failure so the diagnostic doesn't mislead when both gates exist.
    CeilingUnset { which_env: &'static str },
    /// R-21 — cumulative run spend would push past the configured
    /// `SWFC_AWS_RUN_TOTAL_CEILING_USD` (default 100 USD). A single
    /// provision may stay under the per-launch ceiling but
    /// reprovision loops (spot reclaim, capacity rebalance, manual
    /// retry) can quietly accrue past $100; this stops the loop.
    CumulativeCeilingExceeded {
        /// Total spend accrued by prior provisions in this run (USD).
        cumulative_usd: f64,
        /// Estimated spend for the next provision (USD).
        next_estimate_usd: f64,
        /// Run-total ceiling in USD from `SWFC_AWS_RUN_TOTAL_CEILING_USD`.
        ceiling_usd: f64,
    },
    /// R-21 — the cumulative-spend sidecar exists but contains
    /// unreadable / unparseable content. Fail closed rather than
    /// silently reset the running total to zero (which would let a
    /// long-running session re-cross the ceiling).
    CumulativePersistenceFailed(String),
}

impl std::fmt::Display for CostGuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CostGuardError::UnknownInstanceType(t) => write!(
                f,
                "no pricing data for instance type '{}' — update crates/harness/src/executor/aws/pricing.rs::INSTANCE_PRICES_USD_PER_HOUR",
                t
            ),
            CostGuardError::NegativeHours(t, h) => write!(
                f,
                "negative hours ({}) for instance type '{}'",
                h, t
            ),
            CostGuardError::CeilingExceeded {
                estimated_usd,
                ceiling_usd,
            } => write!(
                f,
                "estimated spend ${:.2} exceeds SWFC_AWS_COST_CEILING_USD=${:.2}. Set a higher ceiling or reduce the planned run.",
                estimated_usd, ceiling_usd
            ),
            CostGuardError::CeilingUnset { which_env } => write!(
                f,
                "{which_env} is unset (or not a positive number). \
                 AWS provisioning requires an explicit USD ceiling. \
                 Set e.g. `{which_env}=10` before launching the harness.",
                which_env = which_env,
            ),
            CostGuardError::CumulativeCeilingExceeded {
                cumulative_usd,
                next_estimate_usd,
                ceiling_usd,
            } => write!(
                f,
                "cumulative run spend ${:.2} + next provision ${:.2} would exceed \
                 SWFC_AWS_RUN_TOTAL_CEILING_USD=${:.2}. The current run has accrued \
                 too much spend across repeated provisions; raise the ceiling or \
                 stop the run.",
                cumulative_usd, next_estimate_usd, ceiling_usd
            ),
            CostGuardError::CumulativePersistenceFailed(msg) => write!(
                f,
                "could not read/write the cumulative-spend sidecar: {}. \
                 Fail-closed semantics block provisioning until the sidecar \
                 is readable or the operator manually clears it from \
                 ~/.scripps-workflow/cumulative_spend/.",
                msg
            ),
        }
    }
}

impl std::error::Error for CostGuardError {}

// ── R-21 cumulative-spend tracker ────────────────────────────────────
//
// The per-provision ceiling stops a single oversized launch. The
// cumulative tracker stops a long-running session that re-launches
// many small instances and accumulates past the run-total budget.
// Persisted to disk so a harness restart resumes the same accounting
// (otherwise an SME-driven crash-loop would zero the running total
// every restart).
//
// File layout: `~/.scripps-workflow/cumulative_spend/<package_id>.json`
// Format: `{"total_usd_spent_this_run": <f64>}` — single key, easy
// for an operator to `cat` and reset by hand.
//
// Atomic writes: tempfile + rename inside the same directory so a
// partial write never corrupts the persisted total.

/// R-21 — env var name for the cumulative run-total ceiling.
pub const RUN_TOTAL_CEILING_ENV: &str = "SWFC_AWS_RUN_TOTAL_CEILING_USD";

/// Companion to `RUN_TOTAL_CEILING_ENV` — the per-provision ceiling.
/// Both share `CostGuardError::CeilingUnset`; the variant carries
/// `which_env` so the diagnostic names whichever variable is missing.
pub const PER_PROVISION_CEILING_ENV: &str = "SWFC_AWS_COST_CEILING_USD";

/// R-21 — default cumulative ceiling when the env var is unset. Picked
/// to match the per-provision default surface area: most real-world
/// runs spend tens of dollars, $100 is the "is this still a sane run"
/// alarm threshold.
pub const DEFAULT_RUN_TOTAL_CEILING_USD: f64 = 100.0;

/// R-21 — cumulative-spend tracker. Maintains a persisted total per
/// package id and gates new provisions against the configured run-
/// total ceiling. Construct via `CumulativeSpend::for_package(...)`.
///
/// Lifetime: usually one per AwsExecutor / package; method calls are
/// short-lived (read sidecar → check ceiling → write sidecar). No
/// threading model — the harness loop calls this synchronously from
/// `do_provision`.
#[derive(Debug, Clone)]
pub struct CumulativeSpend {
    sidecar: PathBuf,
    ceiling_usd: f64,
}

impl CumulativeSpend {
    /// R-21 — construct a tracker for `package_id`. Resolves the
    /// sidecar path under `$HOME/.scripps-workflow/cumulative_spend/`
    /// and reads the run-total ceiling from
    /// `SWFC_AWS_RUN_TOTAL_CEILING_USD` (default $100). Creates the
    /// directory lazily on first write.
    pub fn for_package(package_id: &str) -> Self {
        Self::with_root(default_cumulative_root(), package_id)
    }

    /// R-21 — same as `for_package` but with an explicit root
    /// directory. Used by tests to point at a tempdir without
    /// touching the operator's real `$HOME` state.
    pub fn with_root(root: PathBuf, package_id: &str) -> Self {
        let sanitized = sanitize_package_id(package_id);
        let sidecar = root.join(format!("{}.json", sanitized));
        let ceiling_usd = read_run_total_ceiling();
        Self {
            sidecar,
            ceiling_usd,
        }
    }

    /// W5.1 — fail-closed constructor. Returns `Err(CeilingUnset)` when
    /// `SWFC_AWS_RUN_TOTAL_CEILING_USD` is unset or unparseable. Use
    /// from AWS production paths so a missing ceiling refuses to
    /// provision instead of silently applying the $100 default.
    pub fn for_package_strict(package_id: &str) -> Result<Self, CostGuardError> {
        Self::with_root_strict(default_cumulative_root(), package_id)
    }

    /// W5.1 — fail-closed analog of `with_root`. Same semantics as
    /// `for_package_strict` but with an explicit root directory, so
    /// tests that want to exercise the strict path can target a
    /// tempdir.
    pub fn with_root_strict(root: PathBuf, package_id: &str) -> Result<Self, CostGuardError> {
        let sanitized = sanitize_package_id(package_id);
        let sidecar = root.join(format!("{}.json", sanitized));
        let ceiling_usd = read_run_total_ceiling_strict()?;
        Ok(Self {
            sidecar,
            ceiling_usd,
        })
    }

    /// Run-total ceiling in USD this tracker enforces.
    pub fn ceiling_usd(&self) -> f64 {
        self.ceiling_usd
    }

    /// R-21 — current cumulative spend in USD. Reads the sidecar fresh
    /// each call (cheap, on-disk JSON). Returns 0.0 when the sidecar
    /// does not yet exist (no prior provisions for this package).
    /// Returns `Err(CumulativePersistenceFailed)` when the sidecar is
    /// present but unreadable / unparseable — fail-closed posture so
    /// a corrupted file can't silently reset accounting to zero.
    pub fn current_cumulative(&self) -> Result<f64, CostGuardError> {
        match std::fs::read_to_string(&self.sidecar) {
            Ok(raw) => {
                let v: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
                    CostGuardError::CumulativePersistenceFailed(format!(
                        "parse {}: {}",
                        self.sidecar.display(),
                        e
                    ))
                })?;
                let total = v
                    .get("total_usd_spent_this_run")
                    .and_then(|t| t.as_f64())
                    .ok_or_else(|| {
                        CostGuardError::CumulativePersistenceFailed(format!(
                            "missing or non-numeric `total_usd_spent_this_run` in {}",
                            self.sidecar.display()
                        ))
                    })?;
                if !total.is_finite() || total < 0.0 {
                    return Err(CostGuardError::CumulativePersistenceFailed(format!(
                        "non-finite or negative cumulative total ({}) in {}",
                        total,
                        self.sidecar.display()
                    )));
                }
                Ok(total)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0.0),
            Err(e) => Err(CostGuardError::CumulativePersistenceFailed(format!(
                "read {}: {}",
                self.sidecar.display(),
                e
            ))),
        }
    }

    /// R-21 — check whether `next_estimate_usd` is allowed under the
    /// run-total ceiling given the persisted cumulative. Pure read,
    /// no mutation. Use this from `provision()` before launching;
    /// follow with `record_provision` after a successful launch.
    pub fn check_cumulative(&self, next_estimate_usd: f64) -> Result<(), CostGuardError> {
        let cumulative = self.current_cumulative()?;
        if cumulative + next_estimate_usd > self.ceiling_usd {
            return Err(CostGuardError::CumulativeCeilingExceeded {
                cumulative_usd: cumulative,
                next_estimate_usd,
                ceiling_usd: self.ceiling_usd,
            });
        }
        Ok(())
    }

    /// R-21 — add `usd` to the persisted cumulative total. Atomic
    /// (tempfile + rename) so a crash mid-write never leaves the
    /// sidecar in a partial state. Refuses to record a negative or
    /// non-finite amount.
    pub fn record_provision(&self, usd: f64) -> Result<f64, CostGuardError> {
        if !usd.is_finite() || usd < 0.0 {
            return Err(CostGuardError::CumulativePersistenceFailed(format!(
                "refusing to record non-finite / negative spend amount: {}",
                usd
            )));
        }
        let prior = self.current_cumulative()?;
        let new_total = prior + usd;
        if let Some(parent) = self.sidecar.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CostGuardError::CumulativePersistenceFailed(format!(
                    "create_dir_all({}): {}",
                    parent.display(),
                    e
                ))
            })?;
        }
        let payload = serde_json::json!({ "total_usd_spent_this_run": new_total }).to_string();
        // Atomic write: tempfile next to the target + rename. Rename
        // is atomic within the same filesystem; both files live in
        // the same parent directory so this holds for typical $HOME
        // configurations.
        let parent = self.sidecar.parent().ok_or_else(|| {
            CostGuardError::CumulativePersistenceFailed(format!(
                "sidecar path {} has no parent",
                self.sidecar.display()
            ))
        })?;
        let tmp = parent.join(format!(
            ".{}.tmp",
            self.sidecar
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("cumulative_spend")
        ));
        std::fs::write(&tmp, payload).map_err(|e| {
            CostGuardError::CumulativePersistenceFailed(format!("write {}: {}", tmp.display(), e))
        })?;
        std::fs::rename(&tmp, &self.sidecar).map_err(|e| {
            // Best-effort cleanup of the temp file; ignore failures
            // because the rename error itself is the load-bearing
            // diagnostic.
            let _ = std::fs::remove_file(&tmp);
            CostGuardError::CumulativePersistenceFailed(format!(
                "rename {} → {}: {}",
                tmp.display(),
                self.sidecar.display(),
                e
            ))
        })?;
        Ok(new_total)
    }

    /// R-21 — operator-facing reset (e.g. starting a new logical
    /// run on the same package). Removes the sidecar so the next
    /// `current_cumulative` returns 0.0. Not called automatically;
    /// the harness keeps the persisted total across restarts on
    /// purpose so a crash-loop can't drain the budget.
    pub fn reset(&self) -> Result<(), CostGuardError> {
        match std::fs::remove_file(&self.sidecar) {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(CostGuardError::CumulativePersistenceFailed(format!(
                "remove {}: {}",
                self.sidecar.display(),
                e
            ))),
        }
    }
}

/// R-21 — default root for cumulative-spend sidecars:
/// `$HOME/.scripps-workflow/cumulative_spend/`. Falls back to the
/// current working directory if `$HOME` is unset (rare; CI containers
/// occasionally elide it).
fn default_cumulative_root() -> PathBuf {
    if let Ok(home) = env::var("HOME") {
        Path::new(&home)
            .join(".scripps-workflow")
            .join("cumulative_spend")
    } else {
        PathBuf::from(".scripps-workflow/cumulative_spend")
    }
}

/// R-21 — package ids come from package paths, which can include `/`,
/// spaces, etc. Sanitize down to `[A-Za-z0-9._-]` so the sidecar path
/// stays inside the cumulative_spend/ directory.
fn sanitize_package_id(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push_str("unknown_package");
    }
    out
}

/// R-21 — read `SWFC_AWS_RUN_TOTAL_CEILING_USD` with default fallback.
/// Unset / unparseable / non-positive → `DEFAULT_RUN_TOTAL_CEILING_USD`
/// ($100). A zero or negative explicit override is treated as a typo
/// and falls back to the default with a warn.
///
/// **W5.1 deprecation note:** Production callers should prefer
/// [`read_run_total_ceiling_strict`] which is fail-closed (returns
/// `Err(CeilingUnset)` when the env var is missing), matching the
/// per-provision `check_ceiling` semantics. This default-fallback
/// variant is retained for test fixtures + legacy callers that
/// haven't migrated yet; it logs a `tracing::warn!` whenever the
/// silent default fires so deployment drift surfaces in logs.
fn read_run_total_ceiling() -> f64 {
    match env::var(RUN_TOTAL_CEILING_ENV) {
        Ok(raw) => match raw.trim().parse::<f64>() {
            Ok(v) if v > 0.0 && v.is_finite() => v,
            _ => {
                tracing::warn!(
                    env = RUN_TOTAL_CEILING_ENV,
                    value = %raw,
                    default = DEFAULT_RUN_TOTAL_CEILING_USD,
                    "non-positive / unparseable run-total ceiling; falling back to default"
                );
                DEFAULT_RUN_TOTAL_CEILING_USD
            }
        },
        Err(_) => {
            // W5.1: surface the silent-default as a tracing::warn! so
            // operators see it in the harness log. The default itself is
            // retained so this is not a hard breaking change; production
            // call sites now go through `read_run_total_ceiling_strict`.
            tracing::warn!(
                env = RUN_TOTAL_CEILING_ENV,
                default = DEFAULT_RUN_TOTAL_CEILING_USD,
                "SWFC_AWS_RUN_TOTAL_CEILING_USD unset; falling back to default \
                 (production deployments should set this explicitly — strict mode \
                 will refuse provisioning)"
            );
            DEFAULT_RUN_TOTAL_CEILING_USD
        }
    }
}

/// W5.1 — fail-closed variant of [`read_run_total_ceiling`]. Returns
/// `Err(CeilingUnset)` when the env var is missing or unparseable,
/// matching the per-provision `check_ceiling` semantics. Production
/// constructors (`CumulativeSpend::for_package_strict`,
/// `with_root_strict`) call this so a missing
/// `SWFC_AWS_RUN_TOTAL_CEILING_USD` aborts provisioning at startup
/// rather than letting a silent $100 default ship to AWS.
fn read_run_total_ceiling_strict() -> Result<f64, CostGuardError> {
    match env::var(RUN_TOTAL_CEILING_ENV) {
        Ok(raw) => match raw.trim().parse::<f64>() {
            Ok(v) if v > 0.0 && v.is_finite() => Ok(v),
            _ => Err(CostGuardError::CeilingUnset {
                which_env: RUN_TOTAL_CEILING_ENV,
            }),
        },
        Err(_) => Err(CostGuardError::CeilingUnset {
            which_env: RUN_TOTAL_CEILING_ENV,
        }),
    }
}

// ── BackendKind + free dispatch ─────────────────────────────────────
//
// R2-N21 — the original design held a `Box<dyn CostModel>`
// inside each executor with `AwsCostModel` / `NoopCostModel` as the two
// production impls. The trait + two struct types proved to be forced
// abstraction: the call sites only ever multiplex on "AWS or not-AWS",
// and the two impls together exactly match a 2-arm match. The trait
// + the `NoopCostModel` + `AwsCostModel` types are deleted; executors
// carry a `BackendKind` and the free function `estimate_for_backend` /
// `check_ceiling_for_backend` matches on it. New backends add a variant.
//
// The AWS-specific free functions above (`estimate_run_cost_usd`,
// `check_ceiling`) are unchanged — `estimate_for_backend` delegates
// to them for the `Aws` arm.

/// Which backend's cost-guard semantics to use.
///
/// `Aws` — full pricing-table estimate + `SWFC_AWS_COST_CEILING_USD`
/// ceiling check. `NoEstimate` — backends that don't model $/hr cost
/// (SLURM where the posture is fairshare/QoS quotas, local where there
/// is no infra spend). Returns `Ok(None)` from `estimate_for_backend`
/// and `Ok(())` from `check_ceiling_for_backend` regardless of input.
///
/// A future `Gcp` / `Azure` / `FairshareSlurm` variant slots in here
/// with a matching arm in both free functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// AWS EC2 executor; estimates cost from the pricing table and enforces `SWFC_AWS_COST_CEILING_USD`.
    Aws,
    /// Executor that doesn't model per-hour cost (SLURM, local). Cost checks are no-ops.
    NoEstimate,
}

/// Estimate planned spend in USD for `tasks = [(instance_type, hours),
///...]` under the given pricing source, dispatched by backend.
/// Returns `Ok(None)` for backends that don't model $/hr cost.
pub fn estimate_for_backend(
    backend: BackendKind,
    tasks: &[(String, f64)],
    source: PricingSource,
) -> Result<Option<f64>, CostGuardError> {
    match backend {
        BackendKind::Aws => estimate_run_cost_usd(tasks, source).map(Some),
        BackendKind::NoEstimate => Ok(None),
    }
}

/// Enforce the backend's ceiling against an estimate, dispatched by
/// backend. No-op for `NoEstimate` or when `estimated_usd` is `None`.
pub fn check_ceiling_for_backend(
    backend: BackendKind,
    estimated_usd: Option<f64>,
) -> Result<(), CostGuardError> {
    match (backend, estimated_usd) {
        (BackendKind::Aws, Some(v)) => check_ceiling(v),
        (BackendKind::Aws, None) => Ok(()),
        (BackendKind::NoEstimate, _) => Ok(()),
    }
}

/// Read the per-provision ceiling from `SWFC_AWS_COST_CEILING_USD`.
/// Returns `None` when the env var is unset or non-positive (same
/// conditions that `check_ceiling` treats as `CeilingUnset`). Used by
/// the emission site so the `CostGuardSnapshot` can include the ceiling
/// that was actually enforced.
pub fn read_provision_ceiling_usd() -> Option<f64> {
    match std::env::var("SWFC_AWS_COST_CEILING_USD") {
        Ok(raw) => match raw.trim().parse::<f64>() {
            Ok(v) if v > 0.0 && v.is_finite() => Some(v),
            _ => None,
        },
        Err(_) => None,
    }
}

/// Emit a `cost_guard_passed` progress event after all cost-guard checks
/// have passed for a planned provision. Packages the per-provision estimate
/// and the up-to-date cumulative spend into a `CostGuardSnapshot` so
/// operators see status on every successful check, not only on abort.
///
/// `task_id` identifies the task being provisioned for; pass `""` when the
/// check fires at the pre-loop provisioning step where no single task is
/// in scope yet.
pub fn emit_cost_guard_passed(
    pc: &crate::progress_client::ProgressClient,
    task_id: &str,
    estimated_usd: f64,
    ceiling_usd: f64,
    cumulative_usd: f64,
    total_ceiling_usd: f64,
) {
    pc.cost_guard_passed(
        task_id,
        crate::progress_client::CostGuardSnapshot {
            estimated_usd,
            ceiling_usd,
            cumulative_usd,
            total_ceiling_usd,
        },
    );
}

#[cfg(test)]
mod tests {
    // S5.32: workspace lint is `unsafe_code = "deny"`. Test code uses
    // `unsafe { std::env::set_var / remove_var }` (unsafe in Rust 2024
    // edition because the env table is not thread-safe). All call sites
    // are single-threaded test setup; bounded waiver scoped to this
    // `mod tests` block.
    #![allow(unsafe_code)]
    use super::*;
    use crate::progress_client::ProgressClient;

    /// `emit_cost_guard_passed` must enqueue a `cost_guard_passed` event
    /// without dropping it (queue not saturated) and the `CostGuardSnapshot`
    /// must carry the exact values passed in. We verify by pointing the
    /// client at a local TCP listener that captures the POST body, then
    /// asserting the serialised JSON contains the expected `cost_guard` fields.
    #[test]
    fn cost_guard_emits_event_on_pass() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).expect("set non-blocking");

        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = captured.clone();
        let shutdown = Arc::new(Mutex::new(false));
        let shutdown_clone = shutdown.clone();

        let _server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if *shutdown_clone.lock().unwrap() {
                    return;
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                        stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
                        let mut reader = BufReader::new(&mut stream);
                        let mut _first_line = String::new();
                        let _ = reader.read_line(&mut _first_line);
                        let mut content_length: usize = 0;
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).is_err() {
                                break;
                            }
                            let trimmed = line.trim_end_matches(['\r', '\n']);
                            if trimmed.is_empty() {
                                break;
                            }
                            if let Some(v) = trimmed
                                .strip_prefix("Content-Length:")
                                .or_else(|| trimmed.strip_prefix("content-length:"))
                            {
                                content_length = v.trim().parse().unwrap_or(0);
                            }
                        }
                        let mut body = vec![0u8; content_length];
                        if content_length > 0 {
                            let _ = reader.read_exact(&mut body);
                        }
                        captured_clone
                            .lock()
                            .unwrap()
                            .push(String::from_utf8_lossy(&body).to_string());
                        let _ = stream.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => return,
                }
            }
        });

        let pc = ProgressClient::new("session-cg-test", format!("http://127.0.0.1:{}", port));
        emit_cost_guard_passed(&pc, "task_seq_align", 0.42, 10.0, 12.78, 100.0);

        // Wait for the sender thread to drain the event.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if !captured.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        *shutdown.lock().unwrap() = true;
        drop(pc); // bounded join so we don't leak the sender thread

        let log = captured.lock().unwrap();
        assert_eq!(log.len(), 1, "exactly one POST must be captured");
        let body = &log[0];
        let v: serde_json::Value = serde_json::from_str(body).expect("body is valid JSON");

        assert_eq!(v["kind"], "cost_guard_passed");
        assert_eq!(v["task_id"], "task_seq_align");
        assert_eq!(v["status"], "ok");
        assert!(
            v["detail"].as_str().unwrap_or("").contains("$0.42"),
            "detail must include per-provision estimate: {}",
            v["detail"]
        );
        assert!(
            v["detail"].as_str().unwrap_or("").contains("$12.78"),
            "detail must include cumulative spend: {}",
            v["detail"]
        );
        let cg = &v["cost_guard"];
        assert!(
            (cg["estimated_usd"].as_f64().unwrap() - 0.42).abs() < 1e-9,
            "estimated_usd mismatch"
        );
        assert!(
            (cg["ceiling_usd"].as_f64().unwrap() - 10.0).abs() < 1e-9,
            "ceiling_usd mismatch"
        );
        assert!(
            (cg["cumulative_usd"].as_f64().unwrap() - 12.78).abs() < 1e-9,
            "cumulative_usd mismatch"
        );
        assert!(
            (cg["total_ceiling_usd"].as_f64().unwrap() - 100.0).abs() < 1e-9,
            "total_ceiling_usd mismatch"
        );
    }

    #[test]
    fn on_demand_rate_matches_snapshot_for_common_types() {
        // R-20 — snapshot of the us-east-1 reference table. Update
        // these alongside the pricing-table refresh procedure
        // documented at the top of `aws/pricing.rs`.
        assert_eq!(
            hourly_rate_usd("r6i.4xlarge", PricingSource::OnDemand),
            Some(1.008)
        );
        assert_eq!(
            hourly_rate_usd("c6i.4xlarge", PricingSource::OnDemand),
            Some(0.68)
        );
        assert_eq!(
            hourly_rate_usd("t3.medium", PricingSource::OnDemand),
            Some(0.0416)
        );
    }

    #[test]
    fn spot_rate_is_30_percent_of_on_demand() {
        let od = hourly_rate_usd("r6i.4xlarge", PricingSource::OnDemand).unwrap();
        let spot = hourly_rate_usd("r6i.4xlarge", PricingSource::Spot).unwrap();
        assert!((spot - od * SPOT_DISCOUNT_FRACTION).abs() < 1e-9);
    }

    #[test]
    fn unknown_instance_type_returns_none() {
        assert!(hourly_rate_usd("fake.nonexistent", PricingSource::OnDemand).is_none());
    }

    #[test]
    fn estimate_sums_across_tasks() {
        // Two tasks: 2h on r6i.4xlarge (2 × $1.008) + 1h on g6.xlarge ($0.805)
        let tasks = vec![
            ("r6i.4xlarge".to_string(), 2.0),
            ("g6.xlarge".to_string(), 1.0),
        ];
        let total = estimate_run_cost_usd(&tasks, PricingSource::OnDemand).unwrap();
        let expected = 2.0 * 1.008 + 0.805;
        assert!((total - expected).abs() < 1e-9, "total was {}", total);
    }

    #[test]
    fn estimate_fails_on_unknown_instance_type() {
        let tasks = vec![("bogus.type".to_string(), 1.0)];
        let err = estimate_run_cost_usd(&tasks, PricingSource::OnDemand).unwrap_err();
        matches!(err, CostGuardError::UnknownInstanceType(_));
    }

    #[test]
    fn estimate_fails_on_negative_hours() {
        let tasks = vec![("r6i.4xlarge".to_string(), -1.0)];
        let err = estimate_run_cost_usd(&tasks, PricingSource::OnDemand).unwrap_err();
        matches!(err, CostGuardError::NegativeHours(_, _));
    }

    // ── check_ceiling tests. These mutate env, so keep them isolated. ─

    fn with_ceiling<T>(value: Option<&str>, body: impl FnOnce() -> T) -> T {
        // Serialize on the crate-wide env lock so these tests don't
        // race concurrent AwsExecutor / sizing tests that touch the
        // same SWFC_AWS_* env space.
        let _lock = super::super::SWFC_AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let prior = env::var("SWFC_AWS_COST_CEILING_USD").ok();
        match value {
            Some(v) => unsafe { env::set_var("SWFC_AWS_COST_CEILING_USD", v) },
            None => unsafe { env::remove_var("SWFC_AWS_COST_CEILING_USD") },
        }
        let out = body();
        match prior {
            Some(v) => unsafe { env::set_var("SWFC_AWS_COST_CEILING_USD", v) },
            None => unsafe { env::remove_var("SWFC_AWS_COST_CEILING_USD") },
        }
        out
    }

    #[test]
    fn unset_ceiling_now_fails_closed() {
        // Prior behaviour silently allowed every
        // estimate when the env var was unset. The fail-closed
        // contract now demands an explicit positive ceiling on every
        // AWS provisioning call. Operators that don't want a ceiling
        // must use `BackendKind::NoEstimate` instead (R2-N21).
        with_ceiling(None, || {
            let err = check_ceiling(1.0).unwrap_err();
            assert!(matches!(
                err,
                CostGuardError::CeilingUnset {
                    which_env: PER_PROVISION_CEILING_ENV
                }
            ));
        });
    }

    #[test]
    fn ceiling_rejects_above_threshold() {
        with_ceiling(Some("50"), || {
            let err = check_ceiling(75.0).unwrap_err();
            let msg = format!("{}", err);
            assert!(msg.contains("$50.00"));
            assert!(msg.contains("$75.00"));
        });
    }

    #[test]
    fn ceiling_accepts_at_or_below_threshold() {
        with_ceiling(Some("100"), || {
            assert!(check_ceiling(99.99).is_ok());
            assert!(check_ceiling(100.0).is_ok());
        });
    }

    #[test]
    fn malformed_ceiling_fails_closed() {
        // Parse failure is no longer a silent pass. The
        // operator's typo would have hidden the cost guard entirely;
        // surface it as `CeilingUnset` so provisioning aborts.
        with_ceiling(Some("not-a-number"), || {
            let err = check_ceiling(9999.0).unwrap_err();
            assert!(matches!(
                err,
                CostGuardError::CeilingUnset {
                    which_env: PER_PROVISION_CEILING_ENV
                }
            ));
        });
    }

    #[test]
    fn zero_ceiling_fails_closed() {
        // Zero is no longer a "disabled" sentinel. A
        // legitimate "no ceiling" mode is the `BackendKind::NoEstimate` path.
        with_ceiling(Some("0"), || {
            let err = check_ceiling(1.0).unwrap_err();
            assert!(matches!(
                err,
                CostGuardError::CeilingUnset {
                    which_env: PER_PROVISION_CEILING_ENV
                }
            ));
        });
    }

    #[test]
    fn negative_ceiling_fails_closed() {
        with_ceiling(Some("-100"), || {
            let err = check_ceiling(1.0).unwrap_err();
            assert!(matches!(
                err,
                CostGuardError::CeilingUnset {
                    which_env: PER_PROVISION_CEILING_ENV
                }
            ));
        });
    }

    #[test]
    fn ceiling_unset_error_message_is_actionable() {
        with_ceiling(None, || {
            let err = check_ceiling(1.0).unwrap_err();
            let msg = format!("{}", err);
            assert!(
                msg.contains("SWFC_AWS_COST_CEILING_USD"),
                "diagnostic must name the env var, got: {msg}"
            );
        });
    }

    // ── BackendKind + dispatch tests (R2-N21) ───────────────────────

    #[test]
    fn aws_backend_wraps_free_function_for_known_types() {
        let tasks = vec![("r6i.4xlarge".to_string(), 2.0)];
        let est = estimate_for_backend(BackendKind::Aws, &tasks, PricingSource::OnDemand).unwrap();
        assert!(est.is_some(), "Aws backend should return Some");
        // 2 × $1.008 = $2.016 — matches the R-20 us-east-1 reference rate.
        assert!((est.unwrap() - 2.016).abs() < 1e-9);
    }

    #[test]
    fn aws_backend_propagates_unknown_type_error() {
        let tasks = vec![("bogus.type".to_string(), 1.0)];
        let err =
            estimate_for_backend(BackendKind::Aws, &tasks, PricingSource::OnDemand).unwrap_err();
        assert!(matches!(err, CostGuardError::UnknownInstanceType(_)));
    }

    #[test]
    fn aws_backend_check_ceiling_delegates() {
        with_ceiling(Some("50"), || {
            // None: silent pass (nothing to check).
            assert!(check_ceiling_for_backend(BackendKind::Aws, None).is_ok());
            // Some over: rejected.
            let err = check_ceiling_for_backend(BackendKind::Aws, Some(75.0)).unwrap_err();
            assert!(matches!(err, CostGuardError::CeilingExceeded { .. }));
            // Some under: allowed.
            assert!(check_ceiling_for_backend(BackendKind::Aws, Some(25.0)).is_ok());
        });
    }

    #[test]
    fn no_estimate_backend_returns_none_and_ignores_ceiling() {
        let tasks = vec![("r6i.4xlarge".to_string(), 2.0)];
        let est =
            estimate_for_backend(BackendKind::NoEstimate, &tasks, PricingSource::OnDemand).unwrap();
        assert!(est.is_none(), "NoEstimate backend must never model cost");
        // Even with an env ceiling set, NoEstimate check always passes.
        with_ceiling(Some("1"), || {
            assert!(check_ceiling_for_backend(BackendKind::NoEstimate, Some(9999.0)).is_ok());
            assert!(check_ceiling_for_backend(BackendKind::NoEstimate, None).is_ok());
        });
    }

    #[test]
    fn no_estimate_backend_is_permissive_on_instance_type() {
        // The Aws arm errors on unknown types via the pricing-table
        // lookup; the NoEstimate arm never inspects instance_type.
        let tasks = vec![
            ("bogus.type".to_string(), 1.0),
            ("".to_string(), 0.0),
            ("slurm:normal:compute-07".to_string(), 24.0),
        ];
        assert!(
            estimate_for_backend(BackendKind::NoEstimate, &tasks, PricingSource::OnDemand)
                .unwrap()
                .is_none()
        );
    }

    // ── R-21 CumulativeSpend tests ───────────────────────────────────

    fn with_run_total<T>(value: Option<&str>, body: impl FnOnce() -> T) -> T {
        let _lock = super::super::SWFC_AWS_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let prior = env::var(RUN_TOTAL_CEILING_ENV).ok();
        match value {
            Some(v) => unsafe { env::set_var(RUN_TOTAL_CEILING_ENV, v) },
            None => unsafe { env::remove_var(RUN_TOTAL_CEILING_ENV) },
        }
        let out = body();
        match prior {
            Some(v) => unsafe { env::set_var(RUN_TOTAL_CEILING_ENV, v) },
            None => unsafe { env::remove_var(RUN_TOTAL_CEILING_ENV) },
        }
        out
    }

    #[test]
    fn cumulative_default_ceiling_is_100_when_env_unset() {
        with_run_total(None, || {
            let tmp = tempfile::tempdir().unwrap();
            let cs = CumulativeSpend::with_root(tmp.path().to_path_buf(), "pkg-abc");
            assert!((cs.ceiling_usd() - DEFAULT_RUN_TOTAL_CEILING_USD).abs() < 1e-9);
        });
    }

    #[test]
    fn cumulative_reads_env_override() {
        with_run_total(Some("250.5"), || {
            let tmp = tempfile::tempdir().unwrap();
            let cs = CumulativeSpend::with_root(tmp.path().to_path_buf(), "pkg");
            assert!((cs.ceiling_usd() - 250.5).abs() < 1e-9);
        });
    }

    #[test]
    fn cumulative_zero_returned_when_sidecar_missing() {
        with_run_total(None, || {
            let tmp = tempfile::tempdir().unwrap();
            let cs = CumulativeSpend::with_root(tmp.path().to_path_buf(), "pkg-fresh");
            assert!((cs.current_cumulative().unwrap() - 0.0).abs() < 1e-9);
        });
    }

    #[test]
    fn cumulative_record_provision_persists_atomically() {
        with_run_total(None, || {
            let tmp = tempfile::tempdir().unwrap();
            let cs = CumulativeSpend::with_root(tmp.path().to_path_buf(), "pkg-1");
            let after_first = cs.record_provision(5.0).unwrap();
            assert!((after_first - 5.0).abs() < 1e-9);
            let after_second = cs.record_provision(7.50).unwrap();
            assert!((after_second - 12.5).abs() < 1e-9);
            // Persisted across a fresh tracker for the same package.
            let reread = CumulativeSpend::with_root(tmp.path().to_path_buf(), "pkg-1");
            assert!((reread.current_cumulative().unwrap() - 12.5).abs() < 1e-9);
        });
    }

    #[test]
    fn cumulative_check_under_ceiling_allows() {
        with_run_total(Some("50"), || {
            let tmp = tempfile::tempdir().unwrap();
            let cs = CumulativeSpend::with_root(tmp.path().to_path_buf(), "pkg-2");
            cs.record_provision(10.0).unwrap();
            assert!(cs.check_cumulative(20.0).is_ok());
        });
    }

    #[test]
    fn cumulative_check_blocks_at_ceiling() {
        with_run_total(Some("50"), || {
            let tmp = tempfile::tempdir().unwrap();
            let cs = CumulativeSpend::with_root(tmp.path().to_path_buf(), "pkg-3");
            cs.record_provision(40.0).unwrap();
            let err = cs.check_cumulative(20.0).unwrap_err();
            match err {
                CostGuardError::CumulativeCeilingExceeded {
                    cumulative_usd,
                    next_estimate_usd,
                    ceiling_usd,
                } => {
                    assert!((cumulative_usd - 40.0).abs() < 1e-9);
                    assert!((next_estimate_usd - 20.0).abs() < 1e-9);
                    assert!((ceiling_usd - 50.0).abs() < 1e-9);
                }
                other => panic!("unexpected error variant: {other:?}"),
            }
        });
    }

    #[test]
    fn cumulative_default_ceiling_blocks_at_100_dollars() {
        // R-21 — default is $100; a record + check that crosses it
        // fires CumulativeCeilingExceeded.
        with_run_total(None, || {
            let tmp = tempfile::tempdir().unwrap();
            let cs = CumulativeSpend::with_root(tmp.path().to_path_buf(), "pkg-default");
            cs.record_provision(80.0).unwrap();
            let err = cs.check_cumulative(25.0).unwrap_err();
            assert!(matches!(
                err,
                CostGuardError::CumulativeCeilingExceeded { .. }
            ));
        });
    }

    #[test]
    fn cumulative_negative_amount_refused() {
        with_run_total(None, || {
            let tmp = tempfile::tempdir().unwrap();
            let cs = CumulativeSpend::with_root(tmp.path().to_path_buf(), "pkg-neg");
            let err = cs.record_provision(-5.0).unwrap_err();
            assert!(matches!(
                err,
                CostGuardError::CumulativePersistenceFailed(_)
            ));
        });
    }

    #[test]
    fn cumulative_corrupted_sidecar_fails_closed() {
        with_run_total(None, || {
            let tmp = tempfile::tempdir().unwrap();
            let cs = CumulativeSpend::with_root(tmp.path().to_path_buf(), "pkg-corrupt");
            // Pre-populate with garbage.
            std::fs::create_dir_all(tmp.path()).unwrap();
            std::fs::write(tmp.path().join("pkg-corrupt.json"), "not-json").unwrap();
            let err = cs.current_cumulative().unwrap_err();
            assert!(matches!(
                err,
                CostGuardError::CumulativePersistenceFailed(_)
            ));
        });
    }

    #[test]
    fn cumulative_reset_clears_sidecar() {
        with_run_total(None, || {
            let tmp = tempfile::tempdir().unwrap();
            let cs = CumulativeSpend::with_root(tmp.path().to_path_buf(), "pkg-reset");
            cs.record_provision(15.0).unwrap();
            assert!((cs.current_cumulative().unwrap() - 15.0).abs() < 1e-9);
            cs.reset().unwrap();
            assert!((cs.current_cumulative().unwrap() - 0.0).abs() < 1e-9);
            // Idempotent — reset on already-empty is OK.
            cs.reset().unwrap();
        });
    }

    #[test]
    fn cumulative_sanitizes_package_id_with_slashes() {
        with_run_total(None, || {
            let tmp = tempfile::tempdir().unwrap();
            // Path-like ids must not escape the cumulative_spend dir.
            let cs = CumulativeSpend::with_root(tmp.path().to_path_buf(), "../../etc/passwd");
            cs.record_provision(1.0).unwrap();
            // Sanitized: every non-alphanumeric becomes `_`; sidecar
            // must live inside the root.
            let entries: Vec<_> = std::fs::read_dir(tmp.path())
                .unwrap()
                .filter_map(|e| e.ok())
                .collect();
            assert_eq!(entries.len(), 1, "exactly one sidecar created in root");
            let name = entries[0].file_name();
            let s = name.to_str().unwrap();
            assert!(!s.contains('/'), "sanitized id must not contain slashes");
            assert!(
                !s.contains('.') || s.ends_with(".json"),
                "only `.json` extension dots allowed"
            );
        });
    }

    #[test]
    fn cumulative_invalid_env_falls_back_to_default() {
        with_run_total(Some("not-a-number"), || {
            let tmp = tempfile::tempdir().unwrap();
            let cs = CumulativeSpend::with_root(tmp.path().to_path_buf(), "pkg-bad-env");
            assert!((cs.ceiling_usd() - DEFAULT_RUN_TOTAL_CEILING_USD).abs() < 1e-9);
        });
        with_run_total(Some("-10"), || {
            let tmp = tempfile::tempdir().unwrap();
            let cs = CumulativeSpend::with_root(tmp.path().to_path_buf(), "pkg-bad-env-2");
            assert!((cs.ceiling_usd() - DEFAULT_RUN_TOTAL_CEILING_USD).abs() < 1e-9);
        });
    }
}
