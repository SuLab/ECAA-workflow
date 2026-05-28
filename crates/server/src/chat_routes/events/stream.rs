//! SSE `events_stream` GET handler.
//!
//! Subscribes to the per-session broadcast channel and streams
//! `EnvelopedEvent` events to the client until the channel closes. On
//! `Lagged` (subscriber fell behind the ring buffer) emits a synthetic
//! `resync_required` payload with the dropped-count so the UI can
//! refetch state/transcript/DAG and re-converge.
//!
//! Every wire frame is an `EnvelopedEvent` carrying
//! a monotonic per-session `seq`. The synthetic resync payload mints a
//! fresh seq through `ChatAppState::next_sse_seq` so the client's
//! dedup-by-seq path doesn't trip on it.
//!
//! KeepAlive runs at a 15s interval; the underlying broadcast channel
//! (capacity 256) is created lazily by
//! `ChatAppState::broadcaster_subscribe`, which holds the broadcasters
//! write lock across the get-or-insert + subscribe so a concurrent
//! sink-path `fanout()` cannot publish into the channel before our
//! receiver is registered before any fanout can occur.

use crate::chat_routes::{ChatAppState, EnvelopedEvent, SsePayload};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    response::IntoResponse,
};
use std::convert::Infallible;
use std::time::Duration;
use tokio::sync::broadcast;
use uuid::Uuid;

/// `GET /api/chat/session/:id/events` — subscribe to the per-session SSE broadcast channel.
#[tracing::instrument(skip(app), fields(session_id = %session_id))]
pub async fn events_stream(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> axum::response::Response {
    // F10 (audit): refuse SSE subscriptions for unknown
    // session ids BEFORE allocating a broadcast channel. Without this
    // check a garbage-UUID flood would lazily mint per-id broadcast
    // channels in `broadcaster_subscribe`, exhausting the broadcasters
    // map (memory DoS).
    if app.conversation.get_session(session_id).await.is_none() {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    }

    let mut rx = app.broadcaster_subscribe(session_id).await;

    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(envelope) => {
                    let json = serde_json::to_string(&envelope).unwrap_or_default();
                    yield Ok::<Event, Infallible>(Event::default().data(json));
                }
                // the old behavior silently dropped on Lagged so
                // the UI never knew it had fallen behind the ring
                // buffer. Emit a `resync_required` synthetic event
                // carrying the drop count; the client refetches
                // state/transcript/DAG to re-converge (surface broadcast
                // lag instead of silently breaking the stream).
                Err(broadcast::error::RecvError::Lagged(dropped)) => {
                    tracing::warn!(
                        session_id = %session_id,
                        lagged = dropped,
                        "SSE subscriber lagged; emitting resync_required"
                    );
                    let seq = app.next_sse_seq(session_id);
                    let envelope = EnvelopedEvent {
                        seq,
                        payload: SsePayload::ResyncRequired { dropped },
                    };
                    let json = serde_json::to_string(&envelope).unwrap_or_default();
                    yield Ok::<Event, Infallible>(Event::default().data(json));
                    continue;
                }
                Err(_) => break,
            }
        }
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}
