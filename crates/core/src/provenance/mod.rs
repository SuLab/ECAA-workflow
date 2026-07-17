//! Observed-provenance reconciliation (design §5.2, Phase 2, RCA F5).
//!
//! `crates/core` owns the reconciliation types + authoritative-edge
//! model; `crates/harness` is responsible for capturing the observed
//! reads (later task) and `crates/conversation/src/emit/ro_crate.rs`
//! for consuming the reconciled graph (later task). This module is the
//! pure, deterministic core: given the declared producer→consumer
//! `EdgeContract`s for a task and the files it actually read, decide
//! which declared edge (if any) is the authoritative one.

pub mod observed;

pub use observed::{reconcile, ObservedRead, ReconVerdict};
