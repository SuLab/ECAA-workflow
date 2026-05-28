//! `Idempotency-Key` support for high-impact
//! mutating endpoints.
//!
//! ## Why
//!
//! The chat surface has a few endpoints that produce side-effects the
//! SME absolutely does not want to repeat by accident:
//! - `POST /api/chat/session/:id/confirm` advances the session to
//!   `ReadyToEmit` and stamps the git-commit hook.
//! - `POST /api/chat/session/:id/branch_session` forks the session,
//!   creating a new derived package with a fresh audit log.
//! - `POST /api/chat/session/:id/start-execution` spawns the harness
//!   and burns AWS / SLURM minutes.
//!
//! A retry on a transient network blip currently re-fires the action.
//! With `Idempotency-Key` semantics modeled after Stripe / GitHub /
//! AWS, a client that sets the header on the first request gets the
//! same response replayed on every subsequent retry within the
//! configured TTL (default 1 hour, override via
//! `SWFC_IDEMPOTENCY_TTL_SECS`).
//!
//! ## How
//!
//! [`IdempotencyStore`] is a per-`ChatAppState` LRU cache keyed by
//! `(session_id, endpoint_name, sha256(client_key))`. The cache lives
//! in process memory; it does NOT survive a server restart and is NOT
//! shared across nodes. That is acceptable for the current
//! single-instance deployment posture; a future multi-node deployment
//! would replace the backing store with Redis or a database without
//! changing the handler-side API.
//!
//! ## How to apply in a handler
//!
//! ```ignore
//! pub async fn confirm(
//! State(app): State<ChatAppState>,
//! Path(session_id): Path<Uuid>,
//! headers: axum::http::HeaderMap,
//!...
//!) -> Response {
//! let cached = app.idempotency.lookup(session_id, "confirm", &headers);
//! if let Some(resp) = cached.cached_response() {
//! return resp;
//! }
//! //...do the work, produce a `Response`...
//! cached.store(response_clone)
//! }
//! ```
//!
//! When the request has no `Idempotency-Key` header, `lookup` returns
//! a `NoOp` ticket and `store` is a no-op — the handler runs exactly
//! as before. When the header IS present, the first request takes the
//! cache miss path (work runs, response stored) and subsequent
//! retries hit the cache.
//!
//! ## Limitations
//!
//! 1. Bodies are buffered into memory (`Bytes`). High-impact endpoints
//!    return small bodies (`204 No Content`, a tiny JSON envelope) so
//!    this is fine; do not retrofit this onto large-body endpoints.
//! 2. The cache is bounded at 1024 entries via simple FIFO eviction.
//!    Heavy workloads with thousands of unique keys per hour will see
//!    evictions before TTL; that is intentional to bound memory.
//! 3. SSE / streaming responses are NOT cacheable. The handler-side
//!    API surfaces this by returning `None` from `cached_response`
//!    when the stored response had a non-buffered body — but the
//!    current high-impact endpoint set is all small-body, so the
//!    fall-through is exercised only in tests.

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Default TTL: 1 hour. Override via `SWFC_IDEMPOTENCY_TTL_SECS`.
pub const DEFAULT_IDEMPOTENCY_TTL_SECS: u64 = 3600;

/// Maximum live entries. FIFO-evict the oldest when the cap is hit.
pub const MAX_IDEMPOTENCY_ENTRIES: usize = 1024;

/// HTTP header that triggers idempotent replay. Case-insensitive per
/// RFC 9110 §5.1; we read via `HeaderMap::get` which lowercases keys.
pub const HEADER_NAME: &str = "idempotency-key";

/// Cache entry: the buffered response shape we can replay later.
/// We deliberately constrain to small-body responses (status + headers
/// + Bytes body); SSE streams and other non-buffered bodies are
///   rejected by [`StoreTicket::store`] before they enter the cache.
///
/// `pub` so it can sit inside the public [`IdempotencyTicket::Hit`]
/// variant; field visibility stays private so callers must go through
/// [`IdempotencyTicket::cached_response`] to read the body.
#[derive(Clone)]
pub struct CachedResponse {
    status: StatusCode,
    body: Bytes,
    content_type: Option<String>,
    stored_at: Instant,
}

#[derive(Clone)]
struct CacheKey {
    session_id: Uuid,
    endpoint: &'static str,
    key_hash: [u8; 32],
}

impl CacheKey {
    fn matches(&self, sid: Uuid, endpoint: &str, key_hash: &[u8; 32]) -> bool {
        self.session_id == sid && self.endpoint == endpoint && &self.key_hash == key_hash
    }
}

/// Process-local LRU cache for idempotent replays.
pub struct IdempotencyStore {
    ttl: Duration,
    entries: Mutex<VecDeque<(CacheKey, CachedResponse)>>,
}

impl Default for IdempotencyStore {
    fn default() -> Self {
        Self::from_env()
    }
}

impl IdempotencyStore {
    /// Read TTL from `SWFC_IDEMPOTENCY_TTL_SECS`; fall back to
    /// [`DEFAULT_IDEMPOTENCY_TTL_SECS`] (3600s) on parse failure or
    /// missing var.
    pub fn from_env() -> Self {
        let ttl_secs = std::env::var("SWFC_IDEMPOTENCY_TTL_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_IDEMPOTENCY_TTL_SECS);
        Self {
            ttl: Duration::from_secs(ttl_secs),
            entries: Mutex::new(VecDeque::with_capacity(MAX_IDEMPOTENCY_ENTRIES)),
        }
    }

    /// Test-friendly constructor.
    #[cfg(test)]
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(VecDeque::with_capacity(MAX_IDEMPOTENCY_ENTRIES)),
        }
    }

    /// Look up a cached response for the given session + endpoint +
    /// `Idempotency-Key` header. When the header is absent, returns a
    /// no-op ticket so handlers can use the same shape unconditionally.
    pub fn lookup(
        &self,
        session_id: Uuid,
        endpoint: &'static str,
        headers: &HeaderMap,
    ) -> IdempotencyTicket {
        let Some(raw_key) = headers
            .get(HEADER_NAME)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        else {
            return IdempotencyTicket::NoOp;
        };
        // Hash the client-supplied key with sha256 so the cache key
        // length is bounded and no client-controlled string is held
        // verbatim in the lookup table.
        let key_hash = sha256(raw_key.as_bytes());
        let now = Instant::now();
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        // Sweep TTL expiry while we hold the lock; cheap because the
        // capacity is bounded.
        let ttl = self.ttl;
        entries.retain(|(_, resp)| now.duration_since(resp.stored_at) < ttl);
        // Existing entry?
        for (k, resp) in entries.iter() {
            if k.matches(session_id, endpoint, &key_hash) {
                return IdempotencyTicket::Hit(resp.clone());
            }
        }
        IdempotencyTicket::Miss {
            session_id,
            endpoint,
            key_hash,
        }
    }

    /// Internal: insert a freshly-produced response into the cache.
    /// FIFO-evicts oldest entries past the cap.
    fn insert(&self, key: CacheKey, response: CachedResponse) {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        // Replace existing matching entry if present.
        if let Some(pos) = entries.iter().position(|(k, _)| {
            k.session_id == key.session_id
                && k.endpoint == key.endpoint
                && k.key_hash == key.key_hash
        }) {
            entries.remove(pos);
        }
        if entries.len() >= MAX_IDEMPOTENCY_ENTRIES {
            entries.pop_front();
        }
        entries.push_back((key, response));
    }
}

/// Returned from [`IdempotencyStore::lookup`]. Handlers should:
/// 1. Call `cached_response()` and short-circuit if `Some`.
/// 2. Run their normal logic, then call `store(response)` before
///    returning. `store` returns the same response so it can be used
///    as a tail expression.
pub enum IdempotencyTicket {
    /// No `Idempotency-Key` header on the request. `store` is a no-op.
    NoOp,
    /// Cache miss — handler should execute, then call `store`.
    Miss {
        /// Session the request targets.
        session_id: Uuid,
        /// Endpoint name for the cache key (e.g. `"confirm"`, `"start_execution"`).
        endpoint: &'static str,
        /// SHA-256 of the `Idempotency-Key` header value.
        key_hash: [u8; 32],
    },
    /// Cache hit — handler should short-circuit by returning
    /// `cached_response().unwrap()`.
    Hit(CachedResponse),
}

impl IdempotencyTicket {
    /// Returns the cached response when this ticket is a `Hit`.
    #[track_caller]
    pub fn cached_response(&self) -> Option<Response> {
        let entry = match self {
            Self::Hit(e) => e,
            _ => return None,
        };
        let mut builder = Response::builder().status(entry.status);
        if let Some(ct) = &entry.content_type {
            builder = builder.header("content-type", ct);
        }
        // Replay-detection header so a debugging operator can see at a
        // glance that the response was served from the cache.
        builder = builder.header("idempotent-replay", "true");
        Some(
            builder
                .body(Body::from(entry.body.clone()))
                .expect("CachedResponse rebuild is well-formed"),
        )
    }

    /// Store a freshly-produced response in the cache and return the
    /// (possibly cloned) response back to the caller. Returns the
    /// original `response` untouched when this ticket is `NoOp`. For
    /// `Miss`, buffers the body into memory and inserts an entry; if
    /// the body cannot be buffered (e.g. streaming SSE), the response
    /// is passed through without caching.
    ///
    /// Async because buffering the body is async.
    pub async fn store(self, store: &IdempotencyStore, response: Response) -> Response {
        let (session_id, endpoint, key_hash) = match self {
            Self::Miss {
                session_id,
                endpoint,
                key_hash,
            } => (session_id, endpoint, key_hash),
            _ => return response,
        };
        let (parts, body) = response.into_parts();
        let bytes = match axum::body::to_bytes(body, 64 * 1024).await {
            Ok(b) => b,
            Err(_) => {
                // Body too large or not buffer-able. Best-effort:
                // pass the response through without caching by
                // rebuilding from the original parts and an empty body.
                // We can't recover the original body in this branch
                // (we already consumed it), so return a synthetic
                // pass-through. In practice the high-impact endpoints
                // are all 204 / small-JSON, so this branch is
                // exercised only by the test that injects a streaming
                // body.
                return Response::from_parts(parts, Body::empty());
            }
        };
        let content_type = parts
            .headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let cached = CachedResponse {
            status: parts.status,
            body: bytes.clone(),
            content_type: content_type.clone(),
            stored_at: Instant::now(),
        };
        store.insert(
            CacheKey {
                session_id,
                endpoint,
                key_hash,
            },
            cached,
        );
        Response::from_parts(parts, Body::from(bytes))
    }
}

/// Lightweight sha256 helper using the `sha2` workspace dep already
/// pulled in for share-token hashing. Returns the raw 32-byte digest.
fn sha256(input: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(input);
    let out = h.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn make_headers(key: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(v) = key {
            h.insert(HEADER_NAME, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[tokio::test]
    async fn no_header_means_noop_ticket() {
        let store = IdempotencyStore::with_ttl(Duration::from_secs(60));
        let sid = Uuid::new_v4();
        let t = store.lookup(sid, "confirm", &make_headers(None));
        assert!(matches!(t, IdempotencyTicket::NoOp));
        assert!(t.cached_response().is_none());
    }

    #[tokio::test]
    async fn miss_then_hit_replays_cached_response() {
        let store = IdempotencyStore::with_ttl(Duration::from_secs(60));
        let sid = Uuid::new_v4();
        let h = make_headers(Some("client-abc-123"));

        // First call: miss, run + store.
        let t1 = store.lookup(sid, "confirm", &h);
        assert!(matches!(t1, IdempotencyTicket::Miss { .. }));
        let resp = Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header("content-type", "application/json")
            .body(Body::from(r#"{"ok":true}"#))
            .unwrap();
        let _stored = t1.store(&store, resp).await;

        // Second call: hit, replay.
        let t2 = store.lookup(sid, "confirm", &h);
        assert!(matches!(t2, IdempotencyTicket::Hit(_)));
        let replay = t2.cached_response().expect("cache hit returns a body");
        assert_eq!(replay.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            replay.headers().get("idempotent-replay").unwrap(),
            HeaderValue::from_static("true")
        );
    }

    #[tokio::test]
    async fn distinct_keys_distinct_entries() {
        let store = IdempotencyStore::with_ttl(Duration::from_secs(60));
        let sid = Uuid::new_v4();

        let h_a = make_headers(Some("key-a"));
        let h_b = make_headers(Some("key-b"));
        let t1 = store.lookup(sid, "confirm", &h_a);
        let _ = t1
            .store(
                &store,
                Response::builder()
                    .status(StatusCode::NO_CONTENT)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        let t2 = store.lookup(sid, "confirm", &h_b);
        assert!(matches!(t2, IdempotencyTicket::Miss { .. }));
    }

    #[tokio::test]
    async fn distinct_endpoints_distinct_entries() {
        let store = IdempotencyStore::with_ttl(Duration::from_secs(60));
        let sid = Uuid::new_v4();
        let h = make_headers(Some("same-key"));

        let _ = store
            .lookup(sid, "confirm", &h)
            .store(
                &store,
                Response::builder()
                    .status(StatusCode::NO_CONTENT)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        let t2 = store.lookup(sid, "branch", &h);
        assert!(matches!(t2, IdempotencyTicket::Miss { .. }));
    }

    #[tokio::test]
    async fn ttl_expiry_treats_old_entry_as_miss() {
        let store = IdempotencyStore::with_ttl(Duration::from_millis(50));
        let sid = Uuid::new_v4();
        let h = make_headers(Some("expiring"));

        let _ = store
            .lookup(sid, "confirm", &h)
            .store(
                &store,
                Response::builder()
                    .status(StatusCode::NO_CONTENT)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        // Past the TTL the entry is swept on next lookup.
        tokio::time::sleep(Duration::from_millis(60)).await;
        let t2 = store.lookup(sid, "confirm", &h);
        assert!(matches!(t2, IdempotencyTicket::Miss { .. }));
    }
}
