//! Streaming delta callback type used by the `LlmBackend::send_turn_streaming`
//! path and the `StreamAccumulator`. `Arc` instead of `&dyn Fn` so the
//! async_trait lifetime expansion doesn't fight the borrow checker when the
//! sink is held across await points.

use std::sync::Arc;

/// Callback fired once per text chunk as it arrives from the backend.
pub type DeltaSink = Arc<dyn Fn(&str) + Send + Sync>;
