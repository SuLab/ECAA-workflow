//! legacy-session blocker fallback.
//!
//! `SessionState::resolved_blocker()` bridges pre-PR-2.11.2 sessions
//! (where `Blocked` had only `reason`/`recovery_hint` strings) to the
//! typed `BlockerKind`/`BlockerContext` pair every new caller wants.
//! Eligible for deletion when the 30-day TTL has fully cycled new
//! sessions — isolated here so that removal is a one-file diff.

use super::SessionState;
use scripps_workflow_core::blocker::{BlockerContext, BlockerKind};

impl SessionState {
    /// Resolve the typed `BlockerKind` for a `Blocked` state, falling
    /// back to `BlockerKind::AgentError { message: reason }` for legacy
    /// sessions without the structured field. Returns `None` for non-
    /// `Blocked` states.
    pub fn resolved_blocker(&self) -> Option<(BlockerKind, BlockerContext)> {
        match self {
            SessionState::Blocked {
                blockers,
                reason,
                recovery_hint,
                blocker_kind,
                context,
            } => {
                if let Some(latest) = blockers.last() {
                    let ctx = BlockerContext {
                        timestamp: latest.at.to_rfc3339(),
                        recovery_hints: latest.recovery_hint.clone(),
                    };
                    return Some((latest.kind.clone(), ctx));
                }
                let kind = blocker_kind
                    .clone()
                    .unwrap_or_else(|| BlockerKind::AgentError {
                        message: reason.clone(),
                    });
                let ctx = context.clone().unwrap_or_else(|| BlockerContext {
                    // Legacy sessions don't carry a real blocker
                    // timestamp; using `now_rfc3339()` here would make
                    // the same call return a different value on every
                    // invocation, defeating downstream equality checks
                    // and byte-reproducible logs. Use the Unix epoch as
                    // an explicit sentinel — callers that care about
                    // "real" timestamps already check `blockers.last()`
                    // first and skip this branch.
                    timestamp: "1970-01-01T00:00:00+00:00".to_string(),
                    recovery_hints: if recovery_hint.is_empty() {
                        None
                    } else {
                        Some(recovery_hint.clone())
                    },
                });
                Some((kind, ctx))
            }
            _ => None,
        }
    }
}
