//! Provider-neutral LLM abstraction layer (R2-N19).
//!
//! The canonical `LlmBackend` trait lives here under
//! `crate::llm::backend`. Provider-specific implementations
//! (currently only Anthropic) live in their own submodules — see
//! [`crate::anthropic`] for the live HTTP client and [`crate::mock`]
//! for the offline `MockLlmBackend`.
//!
//! The trait's request/response shapes are still Anthropic-flavoured
//! (`TurnRequest` / `TurnResponse` / `DeltaSink` re-exported from
//! [`crate::anthropic`]); neutralising those into a provider-agnostic
//! data model is a follow-up PR. For now, moving the trait file gives
//! us the canonical import path without changing the trait's shape.

pub mod backend;

pub use backend::LlmBackend;
