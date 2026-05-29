// Miscellaneous integration tests: fixtures, metrics, pruning, prompt hashing.
// Each submodule corresponds to a formerly top-level tests/*.rs file.
// `common` is declared once here and shared across submodules that need it.

#[path = "../common/mod.rs"]
mod common;

mod fixture_runner;
mod metrics_recorders_persist;
mod prompt_role_hash_stable;
mod prune_clears_in_memory_maps;
