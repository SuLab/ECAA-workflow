//! Observed-provenance reconciliation (design §5.2, Phase 2, RCA F5).
//!
//! `crates/core` owns the reconciliation types + authoritative-edge
//! model (this module, plus [`crate::ro_crate::reconcile_ro_crate_edges`]);
//! `crates/harness` captures the observed reads onto
//! `runtime/invocations.jsonl` (`invocation_log::InvocationRecord::observed_reads`,
//! `observed_reads::capture_reads`) and `crates/conversation/src/emit/ro_crate.rs`
//! consumes the reconciled graph, stamping the RO-Crate's
//! `ParameterConnection` nodes with the outcome. This module is the
//! pure, deterministic core: given the declared producer→consumer
//! `EdgeContract`s for a task and the files it actually read, decide
//! which declared edge (if any) is the authoritative one.

pub mod observed;

pub use observed::{reconcile, DivergenceRecord, ObservedRead, ReconVerdict};
