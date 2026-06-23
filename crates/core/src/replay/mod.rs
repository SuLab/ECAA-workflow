//! Agent-free replay: re-verify + re-execute a downloaded ECAA package.
pub mod report;
pub mod reverify;
pub mod select;
pub use report::{ReplayReport, ReplayVerdict, ReverifyResult, ReexecuteResult, VerifierDiff, SkippedStage, compute_verdict};
