// Server-Sent Events stream for the LLM chat session.
// Mirrors the SsePayload enum in crates/server/src/chat_routes.rs.

import type { SessionState, Turn } from '../types'
import type { ArtifactRef } from './chatClient'

type ChatSsePayload =
  | { type: 'tool_call_started'; tool_name: string; status_line: string }
  | { type: 'tool_call_finished'; tool_name: string }
  | { type: 'assistant_token_delta'; text: string }
  | { type: 'state_advanced'; new_state: SessionState }
  | {
      type: 'harness_progress'
      kind: string
      task_id: string
      status: string
      detail: string
      // When the harness is running on a remote backend, the SSE event
      // carries the executor metadata so the Jobs tab can render a
      // backend badge + sizing chip per event.
      remote?: {
        backend: string
        instance_id: string
        instance_type: string
      } | null
    }
  | { type: 'infra_error'; reason: string; user_copy: string }
  | {
      // Package re-emission from amend_stage_method.
      type: 'package_amended'
      session_id: string
      amended_stage: string
      invalidated_tasks: string[]
      package_path: string
    }
  | {
      // Fires alongside a task_completed harness_progress when the
      // harness reports artifacts, so the UI can mark that task's card
      // reviewable.
      type: 'task_completed_reviewable'
      task_id: string
      artifacts: ArtifactRef[]
    }
  // Pilot lifecycle, stall signals, cross-version diff. Payload shapes
  // mirror crates/server/src/chat_routes.rs SsePayload.
  | { type: 'harness_sizing_pilot_started' }
  | { type: 'harness_sizing_pilot_complete'; report: unknown }
  | { type: 'harness_sizing_pilot_skipped'; reason: string }
  | {
      type: 'harness_stall_detected'
      task_id: string
      signal: unknown
      suggested_action: 'resize' | 'retry' | 'abort'
    }
  | {
      type: 'harness_resize_recommended'
      task_id: string
      from_instance_type: string
      to_instance_type: string
    }
  | { type: 'harness_version_diff'; report: unknown }
  // Emitted when the server's broadcast channel dropped events on this
  // subscriber. `dropped` is the count the server skipped since the
  // last successful delivery. UI refetches state/transcript/DAG to
  // re-converge with the server.
  | { type: 'resync_required'; dropped: number }
  // Auto-fired dashboard summary side-call failed.
  | { type: 'dashboard_summary_failed'; task_id: string; reason: string }
  // A new Turn was committed on the server. UIs append it locally
  // instead of polling getChatTranscript. The Turn shape mirrors
  // `crates/conversation/src/session/state.rs`.
  | { type: 'turn_appended'; turn: Turn }
  // Harness startup diagnostic.
  | {
      type: 'harness_executor_selected'
      name: string
      cpu_budget: number
      gpu_budget: number
      instance_type: string | null
      harness_version: string
      env_mode: string
    }
  // Harness progress-POST health counters.
  | {
      type: 'harness_progress_health'
      total_posts: number
      failed_posts: number
      total_attempts: number
      last_error: string
      last_success_at: string
    }
  // Verified AWS orphan-reap sweep outcome.
  | {
      type: 'harness_orphans_reaped'
      candidate_count: number
      verified_count: number
      unverified_ids: string[]
      policy: string
    }
  // Per-task heartbeat stall advisory.
  | {
      type: 'harness_heartbeat_stalled'
      task_id: string
      age_secs: number
    }
  // A new hypothesized-node proposal was accepted into
  // the session-scoped registry; mounts the HypothesizedProposalCard.
  | {
      type: 'proposal_received'
      proposal_id: string
      node_id: string
    }
  // A validator or sandbox gate just resolved on a proposal.
  // `passed=false` means the proposal transitioned to `Blocked`.
  | {
      type: 'proposal_gate_advanced'
      proposal_id: string
      gate: 'validator' | 'sandbox' | 'sme_signoff'
      passed: boolean
    }
  // SME signoff materialized the proposal into the DAG.
  | {
      type: 'proposal_promoted'
      proposal_id: string
      task_node_id: string
    }
  // SME rejected the proposal. Terminal.
  | {
      type: 'proposal_rejected'
      proposal_id: string
      rationale: string | null
    }

export type ChatSseEvent = ChatSsePayload & { seq?: number }

export function connectChatStream(
  sessionId: string,
  onEvent: (e: ChatSseEvent) => void,
): () => void {
  // Versioned `/api/v1/chat/` mount.
  const es = new EventSource(`/api/v1/chat/session/${sessionId}/events`)
  // EventSource fires `onopen` on the first connection AND on every
  // successful auto-reconnect. The first open is the initial subscribe;
  // each subsequent open means we just recovered from a drop and may
  // have missed events during the gap (e.g., a server restart, or any
  // network blip that lasted longer than the SSE buffer). Synthesize a
  // `resync_required` so the consumer's resyncAll fires — closes the
  // hole where a `state_advanced` (Emitted → Blocked) emitted during
  // the disconnect window would otherwise be lost forever.
  let openCount = 0
  let lastSeq = 0
  // True once any connection has been established. Distinguishes the
  // very first connection (where the first event's seq is not a gap)
  // from a RECONNECT (where a high first-seq means the server advanced
  // while we were disconnected and events were dropped).
  let hadConnection = false
  // Carries the pre-reconnect high-water seq across the lastSeq reset so
  // the first post-reconnect event can compute the real dropped count.
  let lastSeqBeforeReconnect = 0
  es.onopen = () => {
    openCount += 1
    if (openCount > 1) {
      // Reconnect: stash the prior high-water seq, then reset lastSeq so
      // a server that resumes at a low seq (counter reset) is not treated
      // as a giant backward gap. The accurate resync now fires from the
      // first post-reconnect message below; this onopen resync is the
      // floor that fires even if NO further events arrive after the gap.
      lastSeqBeforeReconnect = lastSeq
      lastSeq = 0
      onEvent({ type: 'resync_required', dropped: 0 })
    }
    hadConnection = true
  }

  es.onmessage = (msg: MessageEvent) => {
    try {
      const parsed = JSON.parse(msg.data) as ChatSseEvent
      if (typeof parsed.seq === 'number') {
        // Duplicate / late re-delivery. With server-side mint-at-send
        // the spawned-fanout vs sync-broadcast race no longer inverts
        // seqs, so a `<= lastSeq` event is now genuinely a late dup.
        if (parsed.seq <= lastSeq) return
        if (lastSeq === 0 && hadConnection && openCount > 1) {
          // First event of a RECONNECT. Unlike a fresh first-ever
          // connection (not a gap), a reconnect at a seq above the
          // pre-reconnect high-water mark means the server advanced
          // while we were disconnected — fire a resync with the real
          // dropped count so the consumer refetches state/transcript/DAG.
          if (parsed.seq > lastSeqBeforeReconnect + 1) {
            onEvent({
              type: 'resync_required',
              dropped: parsed.seq - lastSeqBeforeReconnect - 1,
            })
          }
        } else if (lastSeq !== 0 && parsed.seq > lastSeq + 1) {
          // A forward gap (seq > lastSeq + 1) mid-stream means the
          // server-side broadcast dropped events for this subscriber.
          // Keep this event but fire a resync to reconverge.
          onEvent({ type: 'resync_required', dropped: parsed.seq - lastSeq - 1 })
        }
        lastSeq = parsed.seq
        // Once we accept a post-reconnect event the stashed mark is
        // consumed; clear it so a later mid-stream gap uses lastSeq.
        lastSeqBeforeReconnect = 0
      }
      onEvent(parsed)
    } catch {
      // Drop malformed events silently — server may emit keep-alives.
    }
  }

  es.onerror = () => {
    // EventSource auto-reconnects; the resync fires from `onopen` once
    // the new connection is established.
  }

  return () => es.close()
}
