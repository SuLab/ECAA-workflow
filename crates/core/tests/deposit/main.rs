// Consolidated integration-test binary for the deposit-integrity track
// (RCA I-2 seal order + I-7 BagIt evidence coverage). Follows the same
// `tests/<dir>/main.rs` convention as `tests/provenance/`, `tests/policy/`,
// etc. — cargo's target auto-discovery treats this subdirectory as one test
// binary named `deposit`.
mod bagit_coverage;
mod seal_order;
