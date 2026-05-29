// Session-lifecycle integration tests.
// Each submodule corresponds to a formerly top-level tests/*.rs file.
// `common` is declared once here and shared across all submodules that need it.

#[path = "../common/mod.rs"]
mod common;

mod block_from_harness_states;
mod blocker_queue_appends;
mod branch_task_scoped;
mod documented_constants;
mod force_classify_on_first_intake;
mod intake_followup_convergence;
mod no_silent_transition_drops;
mod session_save_update_race;
mod session_transaction;
