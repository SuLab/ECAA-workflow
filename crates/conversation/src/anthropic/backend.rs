//! Backward-compat re-export shim — the canonical `LlmBackend` trait
//! now lives at [`crate::llm::backend`] (R2-N19). This module stays
//! around so existing call sites that import from
//! `crate::anthropic::backend::LlmBackend` (or via the
//! `anthropic::LlmBackend` re-export in [`super`]) keep compiling.

pub use crate::llm::backend::*;
