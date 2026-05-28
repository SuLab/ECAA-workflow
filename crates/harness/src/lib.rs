//! Harness library surface.
//!
//! The harness is primarily a binary (`main.rs`), but we expose the
//! `executor` module here so integration tests in `tests/` can link to
//! the trait, its impls, and the factory without going through the
//! subprocess path.

pub mod _observability;
pub mod cache_eviction;
pub mod constants;
pub mod dag_patch;
pub mod dispatch_wal;
pub mod executor;
pub mod finalize_probe;
pub mod literature_scope;
pub mod literature_validators;
pub mod multiprocess_lock;
pub mod output_size_guard;
pub mod plan_only;
pub mod progress_client;
pub mod renderer_validators;
pub mod required_artifacts;
pub mod resilient_sync;
pub mod safety_render;
pub mod sandbox_enforcer;
pub mod scheduler;
pub mod scratch_cleanup;
pub mod sme_skip;
pub mod stall_relay;
pub mod swfc_io;
pub mod validators;
pub mod watchdog;
pub mod wrroc_validator_impl;
