//! decision-log helpers on `Session`.
//!
//! `record_decision` is the only method today; future decision-list
//! accessors / audit-trail ops belong here too.

use super::Session;
use ecaa_workflow_core::decision_log::{DecisionActor, DecisionRecord, DecisionType};

impl Session {
    /// Append one decision record to the audit trail. Called by the
    /// REST-endpoint checkpoint handlers (`/confirm`, `/reject`, etc.)
    /// and by the mutation-tool handlers. Always emits the timestamp as
    /// "now".
    ///
    /// When the session has an emitted package on disk, also append
    /// the record to `runtime/decisions.jsonl` so the on-disk audit
    /// log stays in sync with the in-memory `decisions` Vec. Without
    /// the disk append, post-emit decisions (Branch / AmendMethod /
    /// Rerun / CrossVersionDiff / sme-selection / unblock rationale /
    /// confirm rationale) lived only in memory and the GET
    /// `/api/chat/session/:id/decisions` endpoint silently missed
    /// them — the Decisions tab showed only the initial confirm
    /// record from emit time. Best-effort; an IO failure logs to
    /// stderr but does not roll back the in-memory append (the
    /// in-memory state is the source of truth, the file mirrors it).
    pub fn record_decision(
        &mut self,
        decision: DecisionType,
        actor: DecisionActor,
        rationale: Option<String>,
    ) {
        self.record_decision_with_ip(decision, actor, rationale, None);
    }

    /// Variant of `record_decision`
    /// that tags the record with the originating client IP. Server-side
    /// handlers extract the address from the `axum` request via
    /// `ConnectInfo<SocketAddr>` (or `X-Forwarded-For` when configured)
    /// and thread it through; LLM-side and harness-side callers pass
    /// `None`. Field is best-effort — the audit deliverable is that the
    /// shape AVAILABLE everywhere; the on-disk JSONL format stays
    /// backward-compatible because `source_ip` is
    /// `#[serde(default, skip_serializing_if = "Option::is_none")]`.
    pub fn record_decision_with_ip(
        &mut self,
        decision: DecisionType,
        actor: DecisionActor,
        rationale: Option<String>,
        source_ip: Option<String>,
    ) {
        let mut record = DecisionRecord::new(self.id.to_string(), decision, actor, rationale);
        record.source_ip = source_ip;
        // Persist BEFORE pushing to memory so a partial state on
        // crash is still recoverable from the file. If the write
        // fails, fall through to the memory append — log-level only.
        if let Some(pkg) = &self.emitted_package_path {
            let path = pkg.join("runtime").join("decisions.jsonl");
            // Best-effort serialize + append. Synchronous fs is used
            // because the call sites are inside ConversationService
            // store updates that hold a write lock; a tokio::fs await
            // here would require pulling the runtime through the
            // store API. The decisions.jsonl file is small (one line
            // per record, < 1KB each) so the blocking IO is bounded.
            if let Ok(line) = serde_json::to_string(&record) {
                use std::io::Write;
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                {
                    Ok(mut f) => {
                        if let Err(e) = writeln!(f, "{}", line) {
                            eprintln!("[decision-log] append failed for {}: {}", path.display(), e);
                        } else {
                            // Plan S2.1 — fdatasync the post-emit
                            // decisions.jsonl append so a kernel crash
                            // mid-confirm/reject/unblock/branch
                            // doesn't lose the audit record. Panic on
                            // fsync error per the PostgreSQL "20-year
                            // fsync bug" lesson — silent failure here
                            // would leave on-disk state inconsistent
                            // With what the user thinks just.
                            f.sync_data().unwrap_or_else(|e| {
                                panic!(
                                    "decision-log fdatasync failed for {}: {}",
                                    path.display(),
                                    e
                                )
                            });
                        }
                    }
                    Err(e) => eprintln!("[decision-log] open failed for {}: {}", path.display(), e),
                }
            }
        }
        self.decisions.push(record);
    }
}
