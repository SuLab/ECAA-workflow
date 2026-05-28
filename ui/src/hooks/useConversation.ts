import { useCallback, useEffect, useRef, useState } from 'react'
import {
  confirmChatSession,
  createChatSession,
  getChatDag,
  getChatState,
  getChatTranscript,
  getExecution,
  rejectChatSession,
  sendChatTurn,
  startExecution,
  unblockChatSession,
  type SessionStateSnapshot,
} from '../api/chatClient'
import type { CheckpointMode, DAG, SessionMode, SessionState, Turn } from '../types'
import {
  CANCELABLE_THINKING_MS,
  EXECUTION_POLL_MS,
  SLOW_THINKING_MS,
  STILL_THINKING_MS,
  TRANSCRIPT_POLL_MS,
  VERY_SLOW_THINKING_MS,
} from '../lib/polling'
import { mergeBy } from './_merge'

/**
 * D8 mitigation — progressive escalation of the in-flight-turn
 * indicator. Without this, the UI stalls silently on a slow Anthropic
 * call and the SME has no signal to act. Stages escalate on a
 * single-timer cadence:
 *
 * - `'idle'`: no turn in flight.
 * - `'thinking'`: turn started <8s ago (no chip yet; the existing
 *   `sending` boolean drives the composer's disabled state).
 * - `'still_working'`: 8–30s — gentle "still thinking" reassurance.
 * - `'slow'`: 30–60s — explicit "Anthropic is slow; this can happen on
 *   complex turns."
 * - `'very_slow'`: 60–90s — "If this is stuck, you can refresh."
 * - `'cancelable'`: >=90s — Cancel button visible. The server-side
 *   `ECAA_ANTHROPIC_TIMEOUT_SECS=180` ceiling will surface a typed
 *   `Backend` error if the call really hung, but at 90s the SME
 *   shouldn't be forced to wait the remaining 90s before they can act.
 */
export type ThinkingStage =
  | 'idle'
  | 'thinking'
  | 'still_working'
  | 'slow'
  | 'very_slow'
  | 'cancelable'

// Guard against late-resolving fetches from a prior
// session or stale poll overwriting fresh state. `isAbortError` is the
// browser-portable equivalent of `e instanceof DOMException` checks;
// node-fetch / undici / jsdom all surface aborts via `.name === 'AbortError'`.
function isAbortError(e: unknown): boolean {
  return e instanceof Error && e.name === 'AbortError'
}

interface UseConversation {
  sessionId: string | null
  turns: Turn[]
  state: SessionStateSnapshot | null
  /// Hoisted DAG — the canonical source of truth for the Plan tab,
  /// CommandPalette task lookups, and TaskDetailDrawer's per-task
  /// blocker gating. Owning it here means resyncAll / refreshCurrentState
  /// / the 60s poll all keep it fresh in lockstep with `state` and
  /// `turns`, instead of each consumer fetching independently and going
  /// stale on its own schedule.
  dag: DAG | null
  sending: boolean
  /// True when the assistant has been working on a single turn for >8s.
  /// Distinct from the 1s tool-call status pill — this is the whole-turn
  /// "Still thinking…" indicator. Derived from `thinkingStage !== 'idle'
  /// && thinkingStage !== 'thinking'` for back-compat with existing
  /// callers; new UI surfaces should read `thinkingStage` directly so
  /// they can render the matching escalation message + Cancel button.
  stillThinking: boolean
  /// D8 mitigation — escalation stage for the in-flight turn. Drives
  /// progressive messaging in `StillThinkingIndicator` so the SME isn't
  /// staring at an unchanging "Still thinking…" while Anthropic is
  /// stalled. See `ThinkingStage` for the per-stage thresholds and the
  /// matching messages.
  thinkingStage: ThinkingStage
  /// D8 mitigation — abort the current turn's HTTP request. Available
  /// once `thinkingStage === 'cancelable'`. Does NOT send a server-side
  /// cancellation (the in-flight Anthropic call continues until its
  /// `ECAA_ANTHROPIC_TIMEOUT_SECS` ceiling fires); it only releases the
  /// UI's busy gate so the SME can retry or send a different message
  /// without waiting on the hung response.
  cancelTurn: () => void
  error: string | null
  /// Names of background fetches that have hit consecutive errors. Set
  /// becomes non-empty when the server is unreachable; clears when the
  /// next poll succeeds. Drives the staleness banner in ConversationPane.
  staleSources: ReadonlySet<string>
  start: () => Promise<void>
  sendTurn: (text: string) => Promise<void>
  /// Opts mirror the `confirmChatSession` body
  /// shape. `mode` locks the session's discipline (exploratory /
  /// confirmatory / hybrid) at first confirm; `checkpointMode` picks
  /// the per-stage review cadence. Both are optional — undefined opts
  /// preserve the legacy zero-body POST.
  confirm: (opts?: { mode?: SessionMode; checkpointMode?: CheckpointMode }) => Promise<void>
  reject: () => Promise<void>
  unblock: (resolution?: 'resize' | 'retry' | 'abort') => Promise<void>
  reset: () => Promise<void>
  /// Drop the current session context and rehydrate from a different
  /// session id. Used by SessionTree when the user clicks a sibling /
  /// branch pill.
  switchToSession: (id: string) => Promise<void>
  /// Force an immediate /state + /dag refetch. Called by useSseChatEvents
  /// on state_advanced events (and on terminal harness_progress events
  /// via onStateAdvanced) so the UI picks up harness-driven Blocked
  /// transitions and per-task state flips without waiting for the
  /// transcript poll. When the caller has the authoritative
  /// `SessionState` payload from a `state_advanced` SSE event, prefer
  /// `applyStateAdvanced` — it sets the state synchronously so the
  /// BlockerCard renders without waiting on the network refetch.
  refreshCurrentState: () => Promise<void>
  /// Apply a `SessionState` from the SSE `state_advanced` event
  /// payload directly to local state, then trigger the standard
  /// refresh for derived data (DAG / progress counters / blocked
  /// task ids). The synchronous setState ensures the UI re-renders
  /// even when the discriminant didn't change (e.g. a second
  /// `task_blocked` event arrives while the session is already
  /// `Blocked` — the server appends to `state.blockers` in place,
  /// and refetch-only consumers can race the next broadcast and
  /// miss the new entry). Idempotent: applying the same state twice
  /// is a no-op because React's `setState` bails on `Object.is`
  /// equality, and a fresh `setState` with a structurally-equal
  /// new reference still triggers re-render so subscribers
  /// (`ConversationPane`'s blockers map) re-evaluate.
  applyStateAdvanced: (newState?: SessionState) => Promise<void>
  /// Force an immediate /dag refetch on its own. Available for callers
  /// that know only the DAG changed (e.g. a per-task action that didn't
  /// touch session-level state).
  refreshDag: () => Promise<void>
  /// Full re-convergence: refetch state + transcript + DAG in parallel.
  /// Called when the SSE channel reports `resync_required` (server
  /// dropped events on our subscriber) and on tab visibilitychange.
  resyncAll: () => Promise<void>
  /// Append a Turn that arrived via the SSE `turn_appended` event.
  /// Idempotent on `turn_id` so duplicates (reconnect / overlap with a
  /// transcript poll) don't double-render.
  appendTurn: (turn: Turn) => void
  markStale: (source: string) => void
  markFresh: (source: string) => void
  /// Derived from a 3s poll of `/api/chat/session/:id/execution`. True
  /// for statuses `running | pausing | paused | stopping`; false when
  /// the server reports `exited` or no handle. Only polled while the
  /// session is `emitted | blocked | amending` — outside that window
  /// there's no harness to observe, so the value is treated as false.
  /// Drives the unconditional `StartExecutionPanel` affordance in
  /// `ConversationPane` (hides as soon as it flips true).
  executionRunning: boolean
  /// POST /api/chat/session/:id/start-execution. Optimistically sets
  /// `executionRunning = true` before the next /execution poll picks
  /// up the real status so the inline panel hides without a 3s gap.
  startExecutionAction: () => Promise<void>
}

export function useConversation(): UseConversation {
  const [sessionId, setSessionId] = useState<string | null>(null)
  const [turns, setTurns] = useState<Turn[]>([])
  const [state, setState] = useState<SessionStateSnapshot | null>(null)
  const [dag, setDag] = useState<DAG | null>(null)
  const [sending, setSending] = useState(false)
  const [thinkingStage, setThinkingStage] = useState<ThinkingStage>('idle')
  // Derive the legacy `stillThinking` boolean from the new stage so
  // existing callers (StillThinkingIndicator, tests) don't need
  // separate plumbing; cleared when `thinkingStage` is idle or
  // pre-8s.
  const stillThinking =
    thinkingStage !== 'idle' && thinkingStage !== 'thinking'
  const [error, setError] = useState<string | null>(null)
  // D8 mitigation — `AbortController` wired into each in-flight chat
  // POST so the `cancelTurn` action can release the UI's busy gate
  // when Anthropic is hung. Holds the controller of the LATEST turn
  // only; an aborted controller's reference is cleared in the finally
  // block (see sendTurn / confirm).
  const turnAbortRef = useRef<AbortController | null>(null)
  const [staleSources, setStaleSources] = useState<Set<string>>(() => new Set())
  const [executionRunning, setExecutionRunning] = useState(false)
  const startedRef = useRef(false)
  // D8 mitigation — array of timer ids that escalate the in-flight-turn
  // chip through 8s/30s/60s/90s thresholds. The previous single-timer
  // shape only set `stillThinking=true` at 8s and never re-fired; the
  // multi-stage chip needs to advance the stage at each threshold AND
  // be cleanly canceled when the turn finishes (or the SME hits
  // Cancel). Stored as an array so a single `clearStageTimers()` is
  // the canonical teardown.
  const stageTimersRef = useRef<number[]>([])
  // Synchronous busy refs gate every mutator. setState alone is
  // insufficient — the second click can arrive BEFORE React commits the
  // first setSending(true), so the closure still sees `sending === false`
  // and falls through. A ref flipped synchronously before the first await
  // closes that race. Each mutator owns its own ref so a user can confirm
  // AND still send a turn (they're independent).
  const sendingRef = useRef(false)
  const confirmingRef = useRef(false)
  const rejectingRef = useRef(false)
  const unblockingRef = useRef(false)
  // Every sendTurn invocation mints a fresh token; the
  // finally block only clears the still-thinking timer when ITS token
  // is still the current one. Without the token, a late-resolving
  // first turn could clear a second turn's pending timer (the
  // cross-turn timer leak).
  const turnTokenRef = useRef(0)
  // Fetch-sequencing refs.
  //
  // `fetchAbortRef` tracks the single in-flight read-wave (state +
  // transcript + dag). A new wave aborts the prior one before kicking
  // its own controller; `switchToSession` and the popstate handler
  // also abort so a slow A-session response can't land after the SME
  // jumped to B.
  //
  // `currentSessionRef` mirrors `sessionId` synchronously for inside-
  // closure equality checks. Without it, the closure captures the
  // *render-time* sessionId; after a switch, the closure's `id` lags
  // the live ref and the late-resolving response writes A's data into
  // B's UI.
  const fetchAbortRef = useRef<AbortController | null>(null)
  const currentSessionRef = useRef<string | null>(null)

  const markStale = useCallback((source: string) => {
    setStaleSources((prev) => {
      if (prev.has(source)) return prev
      const next = new Set(prev)
      next.add(source)
      return next
    })
  }, [])

  const markFresh = useCallback((source: string) => {
    setStaleSources((prev) => {
      if (!prev.has(source)) return prev
      const next = new Set(prev)
      next.delete(source)
      return next
    })
  }, [])

  const clearStageTimers = useCallback(() => {
    for (const id of stageTimersRef.current) {
      window.clearTimeout(id)
    }
    stageTimersRef.current = []
  }, [])

  const startStillThinkingTimer = useCallback(() => {
    clearStageTimers()
    setThinkingStage('thinking')
    // D8 escalation ladder. Each callback only advances the stage —
    // there's no roll-back, and `clearStageTimers()` is the only path
    // back to 'idle' (called from the turn's finally block or from
    // cancelTurn). The thresholds match the SLOW/VERY_SLOW/CANCELABLE
    // constants so adjusting them requires touching exactly one file.
    stageTimersRef.current.push(
      window.setTimeout(() => setThinkingStage('still_working'), STILL_THINKING_MS),
      window.setTimeout(() => setThinkingStage('slow'), SLOW_THINKING_MS),
      window.setTimeout(() => setThinkingStage('very_slow'), VERY_SLOW_THINKING_MS),
      window.setTimeout(() => setThinkingStage('cancelable'), CANCELABLE_THINKING_MS),
    )
  }, [clearStageTimers])

  const clearStillThinkingTimer = useCallback(() => {
    clearStageTimers()
    setThinkingStage('idle')
  }, [clearStageTimers])

  useEffect(() => {
    return () => {
      clearStageTimers()
    }
  }, [clearStageTimers])

  const cancelTurn = useCallback(() => {
    // D8 mitigation — release the UI's busy gate even though the
    // in-flight Anthropic call may still be running on the server. The
    // controller's abort signal (when threaded through chatClient,
    // future work) would cancel the fetch too; for today the UI just
    // detaches and surfaces a user-friendly error so the SME isn't
    // forced to wait for the 180s ceiling.
    if (turnAbortRef.current) {
      turnAbortRef.current.abort()
      turnAbortRef.current = null
    }
    clearStageTimers()
    setThinkingStage('idle')
    sendingRef.current = false
    setSending(false)
    setError(
      'Cancelled — the previous request was taking too long. Try again or rephrase.',
    )
  }, [clearStageTimers])

  // Keep `currentSessionRef` in lockstep with the
  // `sessionId` state slot. Callers compare `currentSessionRef.current
  // === id` AFTER an async fetch resolves to decide whether their
  // response is still authoritative.
  useEffect(() => {
    currentSessionRef.current = sessionId
  }, [sessionId])

  // Abort any in-flight fetch wave when the hook unmounts so promises
  // resolved after the consumer is gone don't fire setState on a dead
  // component (React 18 strict-mode + a slow connection both produced
  // observable "state update on unmounted component" warnings before).
  useEffect(() => {
    return () => {
      fetchAbortRef.current?.abort()
    }
  }, [])

  const refreshState = useCallback(
    async (id: string) => {
      try {
        const s = await getChatState(id)
        // Drop the response if the user has since switched sessions —
        // a stale snapshot can't overwrite the fresh session's state.
        if (currentSessionRef.current !== id) return
        setState(s)
        markFresh('state')
      } catch (e) {
        if (isAbortError(e)) return
        markStale('state')
      }
    },
    [markStale, markFresh],
  )

  const loadDag = useCallback(
    async (id: string) => {
      try {
        const d = await getChatDag(id)
        if (currentSessionRef.current !== id) return
        setDag(d)
        markFresh('dag')
      } catch (e) {
        if (isAbortError(e)) return
        markStale('dag')
      }
    },
    [markStale, markFresh],
  )

  const start = useCallback(async () => {
    if (startedRef.current) return
    startedRef.current = true
    setError(null)
    try {
      // If the URL has ?session=<uuid>, attach to that existing
      // session instead of creating a new one. Lets a running
      // headless harness be observed through the chat UI without
      // forking a second session. The attach path also hydrates
      // the transcript from the server (the greeting + every
      // SME/assistant turn) so the timeline isn't empty on load.
      const url = new URL(window.location.href)
      const attachId = url.searchParams.get('session')
      if (attachId && /^[0-9a-f-]{36}$/.test(attachId)) {
        // Mirror currentSessionRef so refreshState /
        // loadDag below pass their session-guard.
        currentSessionRef.current = attachId
        setSessionId(attachId)
        const { getChatTranscript } = await import('../api/chatClient')
        const transcript = await getChatTranscript(attachId).catch(() => [] as Turn[])
        if (currentSessionRef.current === attachId) {
          setTurns(transcript)
        }
        await Promise.all([refreshState(attachId), loadDag(attachId)])
        return
      }
      // Share-link visitors without `?session=<id>` shouldn't auto-
      // create a new session — they came here to read someone else's.
      if (url.searchParams.has('share_token')) {
        setError('Shared URL missing session id.')
        return
      }
      const resp = await createChatSession({ careful_mode: false })
      currentSessionRef.current = resp.session_id
      setSessionId(resp.session_id)
      setTurns([resp.greeting])
      // Write the session id into ?session= the same way
      // switchToSession does. Without this, a browser refresh
      // before a turn was sent would lose the session, and tests
      // that observe `window.location.search` for the active
      // session id (e.g. parallel Playwright runs that can't trust
      // /api/chat/sessions/recent across concurrent drivers) would
      // see an empty URL.
      if (typeof window !== 'undefined') {
        const u = new URL(window.location.href)
        if (u.searchParams.get('session') !== resp.session_id) {
          u.searchParams.set('session', resp.session_id)
          window.history.replaceState(null, '', u.toString())
        }
      }
      await Promise.all([refreshState(resp.session_id), loadDag(resp.session_id)])
    } catch (e) {
      setError(String(e))
      startedRef.current = false
    }
  }, [refreshState, loadDag])

  const sendTurn = useCallback(
    async (text: string) => {
      // Synchronous ref gate — the second invocation must see the busy
      // flag BEFORE the first invocation's setSending(true) commits.
      // Reading `sending` from a stale closure (a prior render's value)
      // is the double-send bug this closes.
      if (!sessionId || sendingRef.current) return
      sendingRef.current = true
      const token = ++turnTokenRef.current
      const userTurn: Turn = {
        turn_id: crypto.randomUUID(),
        role: 'user',
        content: text,
        intent: null,
        tool_calls: [],
        quick_replies: [],
        confirmation_card: null,
        timestamp: new Date().toISOString(),
      }
      setTurns((prev) => [...prev, userTurn])
      setSending(true)
      setError(null)
      startStillThinkingTimer()
      try {
        // Pass the optimistic user_turn_id so the server-side persisted
        // user Turn shares the same id. The 60s reconciliation poll's
        // `mergeBy(turn_id)` then dedupes correctly — without this
        // handoff, the optimistic UUID and the server-minted UUID never
        // matched and long-lived sessions accumulated duplicate
        // user-turn bubbles.
        const turn = await sendChatTurn(sessionId, text, {
          userTurnId: userTurn.turn_id,
        })
        // Idempotent on turn_id: the SSE `turn_appended` event for this
        // same assistant turn typically arrives BEFORE the POST response
        // resolves, so a bare `[...prev, turn]` append would render the
        // same turn twice (visible jump + React dup-key warning).
        setTurns((prev) =>
          prev.some((t) => t.turn_id === turn.turn_id) ? prev : [...prev, turn],
        )
        await refreshState(sessionId)
      } catch (e) {
        setError(String(e))
      } finally {
        // Only clear the still-thinking timer when OUR token is still
        // current — otherwise a late-resolving turn would clear a
        // newer turn's pending timer (M1 cross-turn timer leak).
        if (turnTokenRef.current === token) {
          clearStillThinkingTimer()
        }
        sendingRef.current = false
        setSending(false)
      }
    },
    [sessionId, refreshState, startStillThinkingTimer, clearStillThinkingTimer],
  )

  const confirm = useCallback(
    async (opts?: { mode?: SessionMode; checkpointMode?: CheckpointMode }) => {
      // The gate must remain set through the entire body —
      // including the nested `(confirmed — please continue)` turn —
      // so a double-click during the long-tailed follow-up doesn't
      // queue a second confirm POST AND a second synthetic turn.
      if (!sessionId || confirmingRef.current) return
      confirmingRef.current = true
      // The synthetic `(confirmed — please continue)` turn must run
      // INSIDE the sendingRef gate, otherwise an SME pressing Enter
      // (or the composer queueing a typed turn) during the follow-up
      // could queue a second sendChatTurn before this one returned.
      // Flip BOTH refs synchronously and clear in the finally so the
      // composer's gate stays armed for the full confirm + follow-up
      // window. Mirrors the sendTurn pattern on line ~283.
      sendingRef.current = true
      setSending(true)
      setError(null)
      startStillThinkingTimer()
      const token = ++turnTokenRef.current
      try {
        await confirmChatSession(sessionId, opts)
        await refreshState(sessionId)
        // Drive a follow-up turn so the assistant can react to the confirm.
        // Mint a client-side user_turn_id so the server-side persisted
        // user Turn (the `(confirmed — please continue)` synthetic) is
        // stamped with the same id — keeps the reconciliation poll's
        // `mergeBy(turn_id)` from double-rendering this turn after
        // long-lived sessions.
        const confirmFollowupTurnId = crypto.randomUUID()
        // Optimistically append the synthetic user turn BEFORE the
        // server call returns its assistant reply. Without this, the
        // 60s reconciliation poll's mergeBy is the only path that adds
        // the user turn to local state — and mergeBy appends
        // remote-only items at the end, so the user turn lands AFTER
        // the assistant-emit turn that this very call appends below.
        // The fix mirrors the sendTurn pattern (line ~305-315): mint
        // the optimistic user turn, append, then sendChatTurn with the
        // shared userTurnId so the 60s poll's mergeBy dedups by id
        // instead of duplicating.
        const optimisticUserTurn: Turn = {
          turn_id: confirmFollowupTurnId,
          role: 'user',
          content: '(confirmed — please continue)',
          intent: null,
          tool_calls: [],
          quick_replies: [],
          confirmation_card: null,
          timestamp: new Date().toISOString(),
        }
        setTurns((prev) =>
          prev.some((x) => x.turn_id === optimisticUserTurn.turn_id)
            ? prev
            : [...prev, optimisticUserTurn],
        )
        const t = await sendChatTurn(sessionId, '(confirmed — please continue)', {
          userTurnId: confirmFollowupTurnId,
        })
        // Idempotent on turn_id — same SSE/POST race as in sendTurn above.
        setTurns((prev) =>
          prev.some((x) => x.turn_id === t.turn_id) ? prev : [...prev, t],
        )
        await refreshState(sessionId)
      } catch (e) {
        setError(String(e))
      } finally {
        if (turnTokenRef.current === token) {
          clearStillThinkingTimer()
        }
        confirmingRef.current = false
        sendingRef.current = false
        setSending(false)
      }
    },
    [sessionId, refreshState, startStillThinkingTimer, clearStillThinkingTimer],
  )

  const reject = useCallback(async () => {
    // Synchronous double-click gate — same shape as confirm.
    if (!sessionId || rejectingRef.current) return
    rejectingRef.current = true
    try {
      await rejectChatSession(sessionId)
      await refreshState(sessionId)
    } catch (e) {
      setError(String(e))
    } finally {
      rejectingRef.current = false
    }
  }, [sessionId, refreshState])

  const unblock = useCallback(
    async (resolution?: 'resize' | 'retry' | 'abort') => {
      // Synchronous double-click gate — same shape as confirm.
      if (!sessionId || unblockingRef.current) return
      unblockingRef.current = true
      try {
        await unblockChatSession(sessionId, resolution ? { resolution } : undefined)
        await refreshState(sessionId)
      } catch (e) {
        setError(String(e))
      } finally {
        unblockingRef.current = false
      }
    },
    [sessionId, refreshState],
  )

  const reset = useCallback(async () => {
    // Abort any in-flight fetch wave so a stale
    // response can't write into the fresh session.
    fetchAbortRef.current?.abort()
    currentSessionRef.current = null
    setSessionId(null)
    setTurns([])
    setState(null)
    setError(null)
    startedRef.current = false
    await start()
  }, [start])

  const switchToSession = useCallback(
    async (id: string) => {
      if (id === sessionId) return
      // Abort any in-flight read-wave (including the
      // 60s reconciliation poll's wave) before changing the session
      // anchor. Without this, a slow A-session response can land
      // after the URL + state already say B, overwriting B's data.
      fetchAbortRef.current?.abort()
      setError(null)
      // Update the ref BEFORE the setState so any synchronous
      // currentSessionRef checks in still-pending closures see the
      // new id immediately (the matching useEffect that mirrors
      // sessionId only runs after React commits the render).
      currentSessionRef.current = id
      setSessionId(id)
      setTurns([])
      setState(null)
      setDag(null)
      startedRef.current = true
      // Push ?session=<id> so a browser refresh (or a bookmark) keeps
      // the SME on the workflow they jumped to. Mirrors the share-link
      // attach-id contract on first-mount in `start()`.
      if (typeof window !== 'undefined') {
        const url = new URL(window.location.href)
        url.searchParams.set('session', id)
        window.history.pushState(null, '', url.toString())
      }
      const ctl = new AbortController()
      fetchAbortRef.current = ctl
      try {
        const [t, s, d] = await Promise.all([
          getChatTranscript(id, { signal: ctl.signal }),
          getChatState(id, { signal: ctl.signal }),
          getChatDag(id, { signal: ctl.signal }).catch(() => null as DAG | null),
        ])
        // Belt-and-braces session guard — a second `switchToSession`
        // mid-await would have aborted us, but the guard makes the
        // contract explicit and survives a future change to the
        // chatClient signal-handling.
        if (currentSessionRef.current !== id) return
        setTurns(t)
        setState(s)
        setDag(d)
      } catch (e) {
        if (isAbortError(e)) return
        setError(String(e))
      }
    },
    [sessionId],
  )

  // Sync state when browser back/forward changes ?session=. Mirrors the
  // ?view= popstate handler in App.tsx — without this, navigating back
  // from a switched-to session would update the URL but leave the in-
  // memory session frozen on the new one.
  useEffect(() => {
    if (typeof window === 'undefined') return
    const handler = () => {
      const next = new URL(window.location.href).searchParams.get('session')
      if (next && /^[0-9a-f-]{36}$/.test(next) && next !== sessionId) {
        // Reuse switchToSession but without the pushState (popstate
        // already moved the URL — we just need to reload data).
        // Abort prior fetch wave so slow responses
        // from the popped-from session can't overwrite the new one.
        fetchAbortRef.current?.abort()
        setError(null)
        currentSessionRef.current = next
        setSessionId(next)
        setTurns([])
        setState(null)
        setDag(null)
        const ctl = new AbortController()
        fetchAbortRef.current = ctl
        Promise.all([
          getChatTranscript(next, { signal: ctl.signal }),
          getChatState(next, { signal: ctl.signal }),
          getChatDag(next, { signal: ctl.signal }).catch(() => null as DAG | null),
        ])
          .then(([t, s, d]) => {
            if (currentSessionRef.current !== next) return
            setTurns(t)
            setState(s)
            setDag(d)
          })
          .catch((e) => {
            if (isAbortError(e)) return
            setError(String(e))
          })
      }
    }
    window.addEventListener('popstate', handler)
    return () => window.removeEventListener('popstate', handler)
  }, [sessionId])

  useEffect(() => {
    if (!sessionId) return
    // 60s reconciliation poll. `turn_appended` and `state_advanced` SSE
    // events deliver real-time updates; this poll is the fallback for
    // events skipped across a reconnect — and the safety net for the
    // (rare) case where SSE is silently disconnected without firing
    // onerror, which we'd otherwise only catch when the user typed
    // something. State + transcript + DAG are fetched in parallel and
    // tracked under independent staleness sources.
    //
    // Each tick mints a fresh AbortController that
    // is stored on `fetchAbortRef`, so a session switch (or the next
    // tick) can cancel the in-flight read-wave. The setState resolvers
    // are gated on `currentSessionRef.current === sessionId` so a
    // late response from a prior session can't leak into the new one;
    // the transcript merge uses `mergeBy(turn_id)` instead of a
    // wholesale replace so optimistic local appends (the SSE handler
    // beats the next poll, the SME just typed) survive the
    // reconciliation.
    const pollSessionId = sessionId
    const interval = setInterval(async () => {
      const ctl = new AbortController()
      // Don't blow away an in-flight `switchToSession` wave if one is
      // still running (theirs took precedence the moment they aborted
      // ours); but DO record ourselves so the NEXT switch aborts us.
      fetchAbortRef.current?.abort()
      fetchAbortRef.current = ctl
      const [tRes, sRes, dRes] = await Promise.allSettled([
        getChatTranscript(pollSessionId, { signal: ctl.signal }),
        getChatState(pollSessionId, { signal: ctl.signal }),
        getChatDag(pollSessionId, { signal: ctl.signal }),
      ])
      if (currentSessionRef.current !== pollSessionId) return
      if (tRes.status === 'fulfilled') {
        setTurns((prev) => mergeBy(prev, tRes.value, 'turn_id'))
        markFresh('transcript')
      } else if (!isAbortError(tRes.reason)) {
        markStale('transcript')
      }
      if (sRes.status === 'fulfilled') {
        setState(sRes.value)
        markFresh('state')
      } else if (!isAbortError(sRes.reason)) {
        markStale('state')
      }
      if (dRes.status === 'fulfilled') {
        setDag(dRes.value)
        markFresh('dag')
      } else if (!isAbortError(dRes.reason)) {
        markStale('dag')
      }
    }, TRANSCRIPT_POLL_MS)
    return () => clearInterval(interval)
  }, [sessionId, markStale, markFresh])

  // 3s execution-status poll. Only fires while the session has an
  // associated harness (post-emit: `emitted | blocked | amending`).
  // Pre-emit there is no /execution handle; post-completion the harness
  // exits and the StartExecutionPanel must hide the "Start" button
  // automatically. `running | pausing | paused | stopping` all count as
  // running for the panel's visibility check; `exited` / null = not
  // running.
  const pollableForExecution =
    state?.state.kind === 'emitted' ||
    state?.state.kind === 'blocked' ||
    state?.state.kind === 'amending'
  useEffect(() => {
    if (!sessionId || !pollableForExecution) {
      // Reset the flag when we leave the polling window so a re-entry
      // (e.g. amend → emitted) doesn't start with stale state.
      setExecutionRunning(false)
      return
    }
    let cancelled = false
    const tick = async () => {
      try {
        const handle = await getExecution(sessionId)
        if (cancelled) return
        const running =
          !!handle &&
          (handle.status === 'running' ||
            handle.status === 'pausing' ||
            handle.status === 'paused' ||
            handle.status === 'stopping')
        setExecutionRunning(running)
      } catch {
        // Non-fatal — leave the last-known value. The next tick retries.
      }
    }
    void tick()
    const interval = window.setInterval(tick, EXECUTION_POLL_MS)
    return () => {
      cancelled = true
      window.clearInterval(interval)
    }
  }, [sessionId, pollableForExecution])

  const startExecutionAction = useCallback(async () => {
    if (!sessionId) return
    // Optimistic flip so StartExecutionPanel hides without waiting for
    // the next 3s /execution poll. If startExecution rejects, the
    // catch resets the flag so the panel comes back.
    setExecutionRunning(true)
    try {
      await startExecution(sessionId)
    } catch (e) {
      setExecutionRunning(false)
      throw e
    }
  }, [sessionId])

  const refreshCurrentState = useCallback(async () => {
    if (!sessionId) return
    // Refresh both state AND dag so the SSE state_advanced handler
    // (and the harness_progress terminal-event refresh path that
    // piggy-backs on it) keeps the DAG aligned with task transitions,
    // not just the session-level state. Refreshing only `state` would
    // leave the DAG stale on task_blocked / task_completed, so the
    // TaskDetailDrawer's BlockerCard would never surface when the SME
    // clicks a blocked node.
    await Promise.all([refreshState(sessionId), loadDag(sessionId)])
  }, [sessionId, refreshState, loadDag])

  const applyStateAdvanced = useCallback(
    async (newState?: SessionState) => {
      if (!sessionId) return
      // The SSE `state_advanced` event carries the server-authoritative
      // `SessionState` (typed as `Blocked { blockers, ... }` etc.). Apply
      // it synchronously to the local `state` snapshot so the next
      // render observes the new state — even when the discriminant
      // didn't change (Blocked → Blocked with an appended blocker entry
      // is the common case during back-to-back `task_blocked` events
      // for sequential discover_* tasks). Without this, only
      // `refreshCurrentState` ran and the second BlockerCard could miss
      // a render when the refetch raced the next broadcast.
      //
      // Merge into the existing `SessionStateSnapshot` so unrelated
      // fields (progress, blocked_tasks, title, etc.) keep their
      // previously-fetched values until the DAG refresh below lands.
      if (newState) {
        setState((prev) =>
          prev ? { ...prev, state: newState } : prev,
        )
        // Refresh only the DAG so per-task state flips
        // (blocked_tasks, progress counters in the Plan tab) catch up.
        // Skip the /state refetch — the SSE payload's new_state IS
        // the server-authoritative state, and refetching can race a
        // stale per-session cache (the `reconciled_progress_cache` in
        // chat_routes/sessions.rs is invalidated AFTER the broadcast,
        // not before, so a refetch landing between the broadcast and
        // the cache invalidation observes the prior state and would
        // overwrite our optimistic apply).
        await loadDag(sessionId)
      } else {
        // SSE payload omitted new_state (forward-compat path or a
        // resync_required follow-up); fall back to the full refetch.
        await refreshCurrentState()
      }
    },
    [sessionId, refreshCurrentState, loadDag],
  )

  const refreshDag = useCallback(async () => {
    if (sessionId) await loadDag(sessionId)
  }, [sessionId, loadDag])

  const resyncAll = useCallback(async () => {
    if (!sessionId) return
    // Abort any prior in-flight read-wave (60s poll
    // tick, lingering switchToSession, an earlier resyncAll). Store
    // OURS on the ref so a session switch that follows can abort us.
    fetchAbortRef.current?.abort()
    const ctl = new AbortController()
    fetchAbortRef.current = ctl
    const fetchSessionId = sessionId
    try {
      const [t, s, d] = await Promise.all([
        getChatTranscript(fetchSessionId, { signal: ctl.signal }),
        getChatState(fetchSessionId, { signal: ctl.signal }),
        getChatDag(fetchSessionId, { signal: ctl.signal }).catch(
          () => null as DAG | null,
        ),
      ])
      // Drop responses for a now-stale session — a switchToSession
      // landing mid-fetch must win.
      if (currentSessionRef.current !== fetchSessionId) return
      setTurns(t)
      setState(s)
      setDag(d)
      markFresh('state')
      markFresh('transcript')
      markFresh('dag')
    } catch (e) {
      if (isAbortError(e)) return
      markStale('state')
      markStale('transcript')
      markStale('dag')
    }
  }, [sessionId, markStale, markFresh])

  // Force a full resync when the tab returns to the foreground. The 60s
  // reconciliation poll is throttled to ≥1/min in hidden tabs (and can
  // freeze entirely under Page Lifecycle), and a hidden EventSource
  // subscription can be silently dropped without firing onerror — both
  // gaps observed when an Emitted → Blocked transition fired while the
  // tab was hidden and the BlockerCard didn't surface until well after
  // the user returned. Cheap to fire on every visibility flip.
  useEffect(() => {
    if (!sessionId) return
    const handler = () => {
      if (typeof document !== 'undefined' && document.visibilityState === 'visible') {
        void resyncAll()
      }
    }
    document.addEventListener('visibilitychange', handler)
    return () => document.removeEventListener('visibilitychange', handler)
  }, [sessionId, resyncAll])

  const appendTurn = useCallback((turn: Turn) => {
    // Idempotent append: drop if we already have this turn_id
    // (SSE overlap with a transcript poll, reconnect replay, etc.).
    setTurns((prev) => {
      if (prev.some((t) => t.turn_id === turn.turn_id)) return prev
      return [...prev, turn]
    })
  }, [])

  return {
    sessionId,
    turns,
    state,
    dag,
    sending,
    stillThinking,
    thinkingStage,
    cancelTurn,
    error,
    staleSources,
    start,
    sendTurn,
    confirm,
    reject,
    unblock,
    reset,
    switchToSession,
    refreshCurrentState,
    applyStateAdvanced,
    refreshDag,
    resyncAll,
    appendTurn,
    markStale,
    markFresh,
    executionRunning,
    startExecutionAction,
  }
}
