// Tool-dispatch and vocabulary integration tests.
// Each submodule corresponds to a formerly top-level tests/*.rs file.
// `common` is declared once here and shared across submodules that need it.

#[path = "../common/mod.rs"]
mod common;

mod agents_md_tool_count_parity;
mod auto_title_inflight_gate;
mod heuristic_batch_proptest;
mod list_atoms;
mod literature_context_disabled;
mod proposal_gate;
mod scorer_dry_run;
mod tool_boundary_adversarial;
mod verify_cache_prefix;
