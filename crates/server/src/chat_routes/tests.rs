//! Cross-cutting tests that don't have a single submodule home live here.
//!
//! Architecture-improvement-plan §4.4 — the bulk of the previous
//! 1,387-LOC `tests.rs` has been split into per-domain `#[cfg(test)] mod
//! tests` blocks inside the matching submodules (branches.rs,
//! sessions.rs, tasks.rs, turns.rs, execution.rs, events.rs). Shared
//! fixtures live in `chat_routes/test_support.rs`.
