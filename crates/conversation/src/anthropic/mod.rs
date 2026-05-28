//! Thin Anthropic Messages API client and the `LlmBackend` trait.
//!
//! Organised into five focused submodules:
//! * [`backend`] — the `LlmBackend` trait
//! * [`client`] — `AnthropicClient`, `TurnRequest`, `TurnResponse`,
//!   `StopReason`, `Usage`, HTTP + SSE transport
//! * [`stream`] — `StreamEvent` + `parse_stream_chunk` + `synthesize_stream`
//! * [`accumulator`] — `StreamAccumulator`
//! * [`delta_sink`] — the `DeltaSink` type alias

pub mod accumulator;
pub mod backend;
pub mod client;
pub mod delta_sink;
pub mod stream;

// StreamEvent, StreamAccumulator, DeltaSink, and the stream
// parse/synthesize helpers are SSE plumbing, not part of the crate's
// public surface. In-crate consumers (service.rs, mock.rs, tests)
// reach them via the direct submodule paths
// (`crate::anthropic::accumulator::StreamAccumulator`, etc.), so no
// top-of-module `pub use` is needed. This keeps them out of both the
// crate's public API and the `anthropic::*` glob.
pub use backend::LlmBackend;
pub use client::{
    build_messages_payload, AnthropicClient, StopReason, ToolChoice, TurnRequest, TurnResponse,
    Usage,
};
