//! Cursor-based pagination for the growing-collection
//! list endpoints.
//!
//! ## Why
//!
//! Several chat-session list endpoints return collections that grow
//! unboundedly over the life of a session:
//! - `GET /api/chat/session/:id/decisions` — every `confirm` /
//!   `amend_stage` / `branch` / etc. record.
//! - `GET /api/chat/session/:id/transcript` — every chat `Turn`.
//! - `GET /api/chat/session/:id/share-tokens` — issued share-tokens.
//! - `GET /api/chat/session/:id/harness-events` — backlog of harness
//!   lifecycle events.
//!
//! After a few weeks of conversation + harness execution against a
//! long-running package the unbounded-array response shapes get into
//! the multi-megabyte range. The UI's initial render slows; clients
//! that just want "the latest N" cannot avoid paying for the whole
//! tail. The Phase-4 polish wraps each of these in a uniform
//! cursor-paginated envelope.
//!
//! ## Wire shape
//!
//! Every paginated endpoint returns:
//!
//! ```json
//! {
//! "data": [...],
//! "next_cursor": "313030",
//! "has_more": true
//! }
//! ```
//!
//! - `data` is a slice of the underlying collection.
//! - `next_cursor` is an opaque (hex) string; `None`/`null` when no
//!   more rows. Clients echo it back via `?cursor=<value>` to fetch
//!   the next page.
//! - `has_more` is `true` when more rows remain after this page.
//!
//! Query parameters:
//! - `?cursor=<opaque>` — empty/absent means "from the start".
//! - `?limit=<n>` — default 100, max 1000. Out-of-range values are
//!   clamped silently (we deliberately don't 400 because pagination
//!   defaults should never block a working client).
//!
//! ## Cursor encoding
//!
//! The cursor is hex-of-decimal-offset bytes: `0` → "30", `100` →
//! "313030", etc. Opaque to the client by contract — the `hex` crate
//! is already a direct dep (share-token hashing) so this avoids
//! pulling in base64 just for pagination. Decoding failures treat the
//! cursor as "from the start" — a fresh client passing garbage gets
//! the first page (same as no cursor at all) rather than a confusing
//! 400. This matches Stripe / GitHub semantics where malformed
//! cursors are tolerated, not weaponized as DoS levers.
//!
//! Future schemes could swap the offset for an opaque
//! sha256(last_id+timestamp+endpoint) without changing the wire shape;
//! the helper API takes a slice + a `Params` and returns the typed
//! `Page<T>` envelope, so the encoding is encapsulated.

use serde::Serialize;
use std::collections::HashMap;

/// Default page size when `?limit=` is absent or zero.
pub(super) const DEFAULT_LIMIT: usize = 100;

/// Hard ceiling on page size. Larger values get silently clamped to
/// this number. 1000 is chosen so a power-user can disable pagination
/// behavior by passing `?limit=1000` on small collections without
/// hitting an error.
pub(super) const MAX_LIMIT: usize = 1000;

/// Parsed `(?cursor=…, ?limit=…)` view of a paginated request.
///
/// Built via [`Params::from_query`], which is tolerant of:
/// - missing / empty cursors → "from the start"
/// - malformed cursors → "from the start" (no 400 to the client)
/// - missing / zero / out-of-range limits → clamped silently
#[derive(Debug, Clone, Copy)]
pub struct Params {
    /// Zero-based start index decoded from the cursor (or 0 when
    /// absent / malformed).
    pub offset: usize,
    /// Clamped page size, always in `1..=MAX_LIMIT`.
    pub limit: usize,
}

impl Params {
    /// Parse a `?cursor=&limit=` pair out of an axum
    /// `Query<HashMap<String,String>>` extractor. Tolerant of every
    /// malformed input by design — see module-level docs.
    pub fn from_query(q: &HashMap<String, String>) -> Self {
        let offset = q
            .get("cursor")
            .and_then(|raw| {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    return None;
                }
                decode_cursor(trimmed)
            })
            .unwrap_or(0);
        let limit = q
            .get("limit")
            .and_then(|s| s.trim().parse::<usize>().ok())
            .map(clamp_limit)
            .unwrap_or(DEFAULT_LIMIT);
        Self { offset, limit }
    }
}

/// Wire envelope returned from every paginated endpoint. The generic
/// is the row type — `serde_json::Value`, `Turn`, `TokenMetadata`,
/// etc. Serializes to:
/// `{ "data": [...], "next_cursor": "…"|null, "has_more": true|false }`.
#[derive(Debug, Clone, Serialize)]
pub struct Page<T> {
    /// Items in this page.
    pub data: Vec<T>,
    /// Opaque cursor for the next page, or `None` when this was the
    /// last page. `#[serde(skip_serializing_if = "Option::is_none")]`
    /// deliberately omitted — the field stays present in the wire
    /// shape as `null` so clients can branch off field presence.
    pub next_cursor: Option<String>,
    /// True when there are additional items after this page.
    pub has_more: bool,
}

impl<T: Clone> Page<T> {
    /// Slice the rows starting at `params.offset` and take at most
    /// `params.limit`. The full-collection length is `rows.len()`;
    /// the returned envelope advances the cursor by `data.len()` when
    /// there are more rows after the slice.
    ///
    /// The caller is responsible for ordering the input slice — this
    /// helper is order-preserving and does not re-sort. Stable
    /// ordering is a requirement of cursor pagination: if the
    /// underlying collection's order changes between requests the
    /// client will skip / duplicate rows.
    pub fn from_slice(rows: &[T], params: Params) -> Self {
        if rows.is_empty() {
            return Self {
                data: Vec::new(),
                next_cursor: None,
                has_more: false,
            };
        }
        let start = params.offset.min(rows.len());
        let take = params.limit;
        let end = start.saturating_add(take).min(rows.len());
        let data: Vec<T> = rows[start..end].to_vec();
        let has_more = end < rows.len();
        let next_cursor = if has_more {
            Some(encode_cursor(end))
        } else {
            None
        };
        Self {
            data,
            next_cursor,
            has_more,
        }
    }
}

fn clamp_limit(n: usize) -> usize {
    if n == 0 {
        DEFAULT_LIMIT
    } else {
        n.min(MAX_LIMIT)
    }
}

/// Encode a zero-based offset into a hex opaque cursor. The wire
/// form is `hex(ascii-decimal-offset)` — e.g. `100` → "313030". The
/// hex wrapper exists so the client treats the cursor as opaque
/// (and so a future migration to an `id+timestamp` scheme is a
/// drop-in).
pub(super) fn encode_cursor(offset: usize) -> String {
    hex::encode(offset.to_string().as_bytes())
}

/// Decode a hex cursor back into an offset. Returns `None` on any
/// malformed input (bad hex, non-UTF-8 bytes, non-numeric content,
/// negative numbers). The handler layer treats `None` as "start from
/// the beginning" — see module-level docs.
pub(super) fn decode_cursor(raw: &str) -> Option<usize> {
    let bytes = hex::decode(raw).ok()?;
    let s = std::str::from_utf8(&bytes).ok()?;
    s.trim().parse::<usize>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_roundtrip() {
        for offset in [0usize, 1, 99, 100, 1234, 1_000_000] {
            let enc = encode_cursor(offset);
            assert_eq!(decode_cursor(&enc), Some(offset));
        }
    }

    #[test]
    fn malformed_cursor_decodes_to_none() {
        // Invalid hex — odd length / illegal char.
        assert_eq!(decode_cursor("not hex!!!"), None);
        // hex of "not a number"
        assert_eq!(decode_cursor("6e6f74206120206e756d626572"), None);
        // hex of "-5"
        assert_eq!(decode_cursor("2d35"), None);
    }

    #[test]
    fn params_from_empty_query_uses_defaults() {
        let q: HashMap<String, String> = HashMap::new();
        let p = Params::from_query(&q);
        assert_eq!(p.offset, 0);
        assert_eq!(p.limit, DEFAULT_LIMIT);
    }

    #[test]
    fn params_clamps_limit() {
        let mut q = HashMap::new();
        q.insert("limit".to_string(), "0".to_string());
        assert_eq!(Params::from_query(&q).limit, DEFAULT_LIMIT);

        q.insert("limit".to_string(), "5000".to_string());
        assert_eq!(Params::from_query(&q).limit, MAX_LIMIT);

        q.insert("limit".to_string(), "250".to_string());
        assert_eq!(Params::from_query(&q).limit, 250);
    }

    #[test]
    fn params_malformed_cursor_starts_at_zero() {
        let mut q = HashMap::new();
        q.insert("cursor".to_string(), "garbage!!!".to_string());
        assert_eq!(Params::from_query(&q).offset, 0);
    }

    #[test]
    fn page_from_slice_first_page() {
        let rows: Vec<u32> = (0..250).collect();
        let params = Params {
            offset: 0,
            limit: 100,
        };
        let page = Page::from_slice(&rows, params);
        assert_eq!(page.data.len(), 100);
        assert_eq!(page.data.first(), Some(&0));
        assert_eq!(page.data.last(), Some(&99));
        assert!(page.has_more);
        assert_eq!(
            decode_cursor(page.next_cursor.as_deref().unwrap()),
            Some(100)
        );
    }

    #[test]
    fn page_from_slice_walks_to_completion() {
        let rows: Vec<u32> = (0..250).collect();
        let mut params = Params {
            offset: 0,
            limit: 100,
        };
        let mut total_collected = 0usize;
        let mut iterations = 0;
        loop {
            iterations += 1;
            let page = Page::from_slice(&rows, params);
            total_collected += page.data.len();
            if page.has_more {
                params.offset = decode_cursor(page.next_cursor.as_deref().unwrap()).unwrap();
            } else {
                assert!(page.next_cursor.is_none());
                break;
            }
            assert!(iterations < 10, "walk must terminate");
        }
        assert_eq!(total_collected, rows.len());
        assert_eq!(iterations, 3, "250 / 100 = 3 pages");
    }

    #[test]
    fn page_empty_collection_returns_empty_page() {
        let rows: Vec<u32> = Vec::new();
        let params = Params {
            offset: 0,
            limit: 100,
        };
        let page = Page::from_slice(&rows, params);
        assert!(page.data.is_empty());
        assert!(!page.has_more);
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn page_offset_past_end_returns_empty_page() {
        let rows: Vec<u32> = (0..10).collect();
        let params = Params {
            offset: 500,
            limit: 100,
        };
        let page = Page::from_slice(&rows, params);
        assert!(page.data.is_empty());
        assert!(!page.has_more);
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn page_exact_boundary_no_more() {
        // 10 rows, limit 10 → exactly one page, no next cursor.
        let rows: Vec<u32> = (0..10).collect();
        let params = Params {
            offset: 0,
            limit: 10,
        };
        let page = Page::from_slice(&rows, params);
        assert_eq!(page.data.len(), 10);
        assert!(!page.has_more);
        assert!(page.next_cursor.is_none());
    }
}
