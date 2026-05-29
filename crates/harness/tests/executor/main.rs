// Shared process-wide lock serializing all env-mutating tests in this
// binary. Before consolidation each file was its own test binary (separate
// process), so per-module statics were sufficient. Now that multiple
// modules share one process we use a single crate-level lock so tests
// in different modules that touch the same env vars (ECAA_LOCAL_SANDBOX,
// ECAA_AWS_*, PATH) cannot race.
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

mod dispatch_gate_fail_closed_budget;
mod executor;
mod executor_safety;
mod harness_pilot_integration_test;
mod install_proxy;
mod multiprocess_lock_test;
mod sandbox_refusal_corpus;
mod sandbox_runner_integration;
#[cfg(feature = "slurm")]
mod slurm_live;
#[cfg(feature = "slurm")]
mod slurm_ssh_mock;
mod stall_monitor_integration_test;
mod stall_watchdog_interaction;
mod wal_multiprocess_race;
