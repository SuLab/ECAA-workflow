//! Iterative repair loop foundation.
//!
//! Classifies execution failures into [`RepairClass`]es, attempts bounded
//! per-class repairs through [`Executor`]s, and routes agentic needs to
//! review via a [`TaskRunner`]. This is the Phase-1 foundation; later phases
//! add the `assess`, `snapshot`, `executors`, and `driver` submodules.
//!
//! Not to be confused with the V4 DAG `crate::repair` registry.

pub mod assess;
pub mod driver;
pub mod executor;
pub mod executors;
pub mod failure;
pub mod provenance;
pub mod runner;
pub mod snapshot;
pub mod status;

pub use assess::{assess_package, claim_failures, invariant_failures, map_repair_action};
pub use driver::run_repair_loop;
pub use executor::{Executor, ExecutorRegistry, RepairOutcome};
pub use failure::{
    default_budget, Failure, FailureSet, FailureSource, FailureStatus, RepairClass,
    GLOBAL_ROUND_CAP,
};
pub use provenance::{append_repair_log, RepairLogEntry};
pub use runner::{RepairDirective, ReviewRoutingRunner, TaskRunner};
pub use snapshot::{Snapshot, Snapshotter};
pub use status::{from_final, RepairStatus, RepairVerdict, ReviewItem};
