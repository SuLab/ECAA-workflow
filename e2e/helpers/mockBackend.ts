/**
 * Installs page.route() handlers for /api/chat/** and /api/v1/chat/** plus
 * a fake EventSource for the matching chat session events endpoint. The test drives beats through
 * `sendUserMessage` on the Chat helper; each /turn POST advances the
 * backend's beat pointer and returns the next canned assistant Turn.
 *
 * No real network is hit. No real server boots. The page never knows
 * the difference.
 */

import type { Page, Route } from '@playwright/test'
import { FAKE_EVENT_SOURCE_INIT_SCRIPT } from './sseStream'
import type {
  Beat,
  MockHypothesizedProposal,
  RecordedProposalAction,
  SessionState,
  SessionStateKind,
  SessionStateSnapshot,
  SessionMetrics,
  SseEvent,
  Turn,
} from './types'

export interface MockBackendOptions {
  /** Beats for the whole scenario — /turn POSTs advance through these in order. */
  beats: Beat[]
  /** Beats to drive after the finale confirm click (post-confirm follow-up turns). */
  afterConfirmBeats?: Beat[]
  /** Initial state before the first /turn — defaults to intake. */
  initialState?: SessionState
  /** Greeting turn content — defaults to the round-9 canonical greeting. */
  greeting?: string
  /** Metrics snapshot returned by /metrics. null returns 404. */
  metrics?: SessionMetrics | null
  /** State the /confirm POST advances to — default ready_to_emit. */
  confirmTarget?: SessionState
  /** State the /reject POST advances to — default intake_followup. */
  rejectTarget?: SessionState
  /** State the /unblock POST advances to — default intake_followup. */
  unblockTarget?: SessionState
  /**
   * Delay the /turn response for the N-th call by this many milliseconds
   * (1-indexed). Use for still-thinking tests that need the in-flight
   * turn to stay pending past the 8 s client-side timer. Unrelated
   * calls are not delayed.
   */
  delayTurn?: { callIndex: number; delayMs: number }
  /**
   * Status codes to return for specific /turn calls (1-indexed).
   * Use for error-recovery tests. Defaults to 200 for any call not
   * listed.
   */
  turnStatusOverrides?: Record<number, number>
  /**
   * Initial list of hypothesized-node proposals the
   * `GET /api/chat/session/:id/proposals` route returns. Tests can
   * mutate via `handle.setProposals(...)` to simulate the server
   * advancing a proposal's lifecycle. Defaults to `[]` so unrelated
   * specs still get an empty list (matches the server's behaviour
   * for a session with no proposals).
   */
  proposals?: MockHypothesizedProposal[]
}

export interface MockBackendHandle {
  sessionId: string
  /** Push an SSE event to the current session via the fake EventSource. */
  pushSseEvent(event: SseEvent): Promise<void>
  /** Push multiple events in sequence (same microtask). */
  pushSseEvents(events: SseEvent[]): Promise<void>
  /** All POST /turn bodies received so far (assertions). */
  recordedTurnMessages(): string[]
  /** Number of /turn POSTs received. */
  turnCount(): number
  /** Override the state snapshot the next /state GET will return. */
  setState(next: SessionState): void
  /** Override the task count the next /state GET will return. */
  setTaskCount(n: number): void
  /** Manually advance to the post-confirm beats — used by Chat.clickConfirm. */
  enterAfterConfirmPhase(): void
  /** Override the metrics snapshot — tests can flip this mid-flow. */
  setMetrics(next: SessionMetrics | null): void
  /** Force the next /turn call to respond with this status code. */
  failNextTurn(status: number): void
  /**
   * Replace the proposal list the
   * `/api/chat/session/:id/proposals` route returns. Use to simulate
   * the server transitioning a proposal across lifecycle states
   * between SSE event pushes.
   */
  setProposals(next: MockHypothesizedProposal[]): void
  /** POST bodies captured from `/signoff` and `/reject` calls. */
  recordedProposalActions(): RecordedProposalAction[]
  /** Remove all route handlers and stop accepting events. */
  dispose(): Promise<void>
}

const DEFAULT_GREETING =
  "Hi — I'll help you plan a bioinformatics analysis. Tell me about what you're trying to do."

const CHAT_ROUTE_PREFIXES = ['/api/chat', '/api/v1/chat'] as const

function chatRoutePatterns(suffix: string): string[] {
  return CHAT_ROUTE_PREFIXES.map((prefix) => `**${prefix}${suffix}`)
}

function isChatUrl(url: string): boolean {
  return /\/api\/(?:v1\/)?chat(?:\/|$)/.test(new URL(url).pathname)
}

async function routeChat(
  page: Page,
  suffix: string,
  handler: Parameters<Page['route']>[1],
): Promise<void> {
  for (const pattern of chatRoutePatterns(suffix)) {
    await page.route(pattern, handler)
  }
}

async function unrouteChat(page: Page, suffix: string): Promise<void> {
  for (const pattern of chatRoutePatterns(suffix)) {
    await page.unroute(pattern).catch(() => {})
  }
}

export async function installMockBackend(
  page: Page,
  opts: MockBackendOptions,
): Promise<MockBackendHandle> {
  await page.addInitScript(FAKE_EVENT_SOURCE_INIT_SCRIPT)

  const sessionId = randomUuid()
  let beatIndex = 0
  let inAfterConfirm = false
  let afterConfirmIndex = 0

  const greeting: Turn = {
    turn_id: randomUuid(),
    role: 'assistant',
    content: opts.greeting ?? DEFAULT_GREETING,
    intent: null,
    tool_calls: [],
    quick_replies: [],
    confirmation_card: null,
    timestamp: new Date().toISOString(),
  }
  const transcript: Turn[] = [greeting]
  const recordedTurnMessages: string[] = []

  let currentState: SessionState = opts.initialState ?? { kind: 'intake' }
  let currentTaskCount = 0
  let currentDag: unknown | null = null
  let userConfirmed = false
  let metrics: SessionMetrics | null = opts.metrics ?? null
  let turnCallCount = 0
  let forceNextTurnStatus: number | null = null
  // Proposal-lifecycle state. Initial value cloned from
  // opts so a test mutating `setProposals` doesn't aliase the caller's
  // array (mirrors `setTaskCount` semantics).
  let proposals: MockHypothesizedProposal[] = [...(opts.proposals ?? [])]
  const recordedProposalActions: RecordedProposalAction[] = []

  const confirmTarget = opts.confirmTarget ?? { kind: 'ready_to_emit' }
  const rejectTarget = opts.rejectTarget ?? { kind: 'intake_followup' }
  const unblockTarget = opts.unblockTarget ?? { kind: 'intake_followup' }

  const snapshot = (): SessionStateSnapshot => ({
    session_id: sessionId,
    state: currentState,
    user_confirmed: userConfirmed,
    last_activity: new Date().toISOString(),
    task_count: currentTaskCount,
    progress: {
      completed: 0,
      ready: 0,
      blocked: currentState.kind === 'blocked' ? 1 : 0,
      pending: currentTaskCount,
    },
  })

  const fulfillJson = async (route: Route, body: unknown, status = 200) =>
    route.fulfill({
      status,
      contentType: 'application/json',
      body: JSON.stringify(body),
    })

  const fulfillNoContent = async (route: Route) =>
    route.fulfill({ status: 204, body: '' })

  const fulfillNotFound = async (route: Route) =>
    route.fulfill({ status: 404, contentType: 'text/plain', body: 'not found' })

  const applyBeatExpectations = (beat: Beat): void => {
    if (beat.state !== undefined) {
      currentState =
        typeof beat.state === 'string'
          ? ({ kind: beat.state } as SessionState)
          : beat.state
    } else if (beat.expect?.stateBadge !== undefined) {
      // Convenience: infer the state from the expected badge when not set
      // explicitly. Blocked states must be set via beat.state because the
      // kind is not enough.
      if (beat.expect.stateBadge !== 'blocked') {
        currentState = { kind: beat.expect.stateBadge } as SessionState
      }
    }
    if (beat.expect?.planTaskCount !== undefined) {
      currentTaskCount = beat.expect.planTaskCount
    }
    if (beat.dag !== undefined) {
      currentDag = beat.dag
      if (currentTaskCount === 0) {
        const maybeTasks = (beat.dag as { tasks?: unknown })?.tasks
        if (maybeTasks && typeof maybeTasks === 'object') {
          currentTaskCount = Object.keys(maybeTasks).length
        }
      }
    }
  }

  const makeAssistantTurn = (beat: Beat): Turn => ({
    turn_id: randomUuid(),
    role: 'assistant',
    content: beat.assistant.content,
    intent: null,
    tool_calls: beat.assistant.tool_calls ?? [],
    quick_replies: beat.assistant.quick_replies ?? [],
    confirmation_card: beat.assistant.confirmation_card ?? null,
    timestamp: new Date().toISOString(),
  })

  // ── Route handlers ──────────────────────────────────────────────────────
  //
  // Playwright runs handlers from most-recently-registered first. We
  // register the catch-all for legacy /api/* routes FIRST so the chat
  // handlers take precedence over it.

  // Catch-all for legacy /api/* routes that the UI fires (e.g.
  // StateInspectorPane's getDag('/api/dag')). Returns 404 so the UI
  // gracefully falls through to "no dag yet" rather than proxying to a
  // non-existent backend.
  await page.route('**/api/**', async (route) => {
    // Let the chat and git handlers run first.
    const url = route.request().url()
    if (isChatUrl(url)) return route.fallback()
    if (url.includes('/api/git/')) return route.fallback()
    return route.fulfill({
      status: 404,
      contentType: 'application/json',
      body: JSON.stringify({ error: 'not mocked' }),
    })
  })

  // /api/git/* — minimum viable surface so GitSettingsPage renders its
  // disabled-default state (heading + sections) instead of an
  // "Error: get config 404" fallback. The Settings page only requires
  // a successful /config GET on mount; it doesn't drive any other git
  // call until the user enables the feature.
  //
  // Provenance is per-package. `GitConfig` has no global `repo_path`
  // field. Status and log endpoints are session-scoped at
  // `/api/git/session/:id/{status,log}` and echo the resolved
  // per-package repo path back on the status response. The SSH-key
  // endpoint lives at `/api/git/keys/ssh`.
  const defaultGitConfig = {
    enabled: false,
    remote_url: null,
    ssh_key_path: null,
    author_name: 'ECAA-workflow',
    author_email: 'ecaa-workflow@example.invalid',
    commit_on_emit: true,
    commit_on_amend: true,
    commit_on_task_completed: false,
    auto_push: false,
    push_timeout_secs: 30,
  }
  await page.route('**/api/git/**', async (route) => {
    const url = route.request().url()
    const method = route.request().method()
    if (method === 'GET' && url.includes('/api/git/config')) {
      return fulfillJson(route, defaultGitConfig)
    }
    if (method === 'PUT' && url.includes('/api/git/config')) {
      return fulfillJson(route, defaultGitConfig)
    }
    // Session-scoped status: /api/git/session/:id/status
    if (
      method === 'GET' &&
      /\/api\/git\/session\/[^/]+\/status$/.test(url)
    ) {
      return fulfillJson(route, {
        repo_path: `/tmp/scripps-mock-git-package/${sessionId}`,
        remote_url: null,
        git_available: true,
        initialized: false,
        last_commit: null,
        dirty_count: 0,
        commit_count: 0,
      })
    }
    // Session-scoped log: /api/git/session/:id/log?limit=N
    if (
      method === 'GET' &&
      /\/api\/git\/session\/[^/]+\/log(\?|$)/.test(url)
    ) {
      return fulfillJson(route, [])
    }
    return route.fulfill({
      status: 404,
      contentType: 'application/json',
      body: JSON.stringify({ error: 'git endpoint not mocked' }),
    })
  })

  // v3 P10 — the UI polls this at mount and swaps the chat composer for
  // the structured-intake form when the assistant is disabled/unavailable.
  // Mocked UI specs exercise the conversational surface, so keep the
  // assistant available regardless of the operator's local dev-stack mode.
  await routeChat(page, '/llm-availability', async (route) => {
    if (route.request().method() !== 'GET') return route.fallback()
    return fulfillJson(route, { kind: 'available' })
  })

  // POST /api/chat/session — create session
  await routeChat(page, '/session', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    await fulfillJson(route, { session_id: sessionId, greeting })
  })

  // All /api/chat/session/:id/* routes
  await routeChat(page, '/session/*/**', async (route) => {
    const url = route.request().url()
    const method = route.request().method()

    // POST /turn — advance beat, return next canned assistant Turn
    if (method === 'POST' && url.endsWith('/turn')) {
      let body: { message: string }
      try {
        body = JSON.parse(route.request().postData() ?? '{}')
      } catch {
        return fulfillJson(route, { error: 'bad body' }, 400)
      }
      recordedTurnMessages.push(body.message)
      turnCallCount += 1
      const thisCallIndex = turnCallCount

      // Error override: if this call has an explicit override or the
      // "fail next turn" flag is set, return the status and bail.
      const override = opts.turnStatusOverrides?.[thisCallIndex]
      if (forceNextTurnStatus !== null) {
        const status = forceNextTurnStatus
        forceNextTurnStatus = null
        return route.fulfill({
          status,
          contentType: 'text/plain',
          body: `mock /turn failure (status ${status})`,
        })
      }
      if (override !== undefined && override !== 200) {
        return route.fulfill({
          status: override,
          contentType: 'text/plain',
          body: `mock /turn override (status ${override})`,
        })
      }

      const beats = inAfterConfirm ? opts.afterConfirmBeats ?? [] : opts.beats
      const idx = inAfterConfirm ? afterConfirmIndex : beatIndex
      if (idx >= beats.length) {
        return fulfillJson(route, { error: 'beats exhausted' }, 500)
      }
      const beat = beats[idx]
      if (inAfterConfirm) afterConfirmIndex += 1
      else beatIndex += 1

      applyBeatExpectations(beat)

      const turn = makeAssistantTurn(beat)
      // Record the user turn too for /transcript.
      transcript.push({
        turn_id: randomUuid(),
        role: 'user',
        content: body.message,
        intent: null,
        tool_calls: [],
        quick_replies: [],
        confirmation_card: null,
        timestamp: new Date().toISOString(),
      })
      transcript.push(turn)

      // Push beat-owned SSE events BEFORE fulfilling the response so the
      // EventSource delivers them while useConversation.sendTurn is still
      // awaiting the /turn promise. This mirrors the real service loop
      // order: SSE fires during the tool loop, the Turn arrives at the end.
      for (const ev of beat.sse ?? []) {
        await pushSseEventToBrowser(page, sessionId, ev)
      }

      // Optional delay for still-thinking tests.
      const delay =
        opts.delayTurn && opts.delayTurn.callIndex === thisCallIndex
          ? opts.delayTurn.delayMs
          : 0
      if (delay > 0) {
        await new Promise((resolve) => setTimeout(resolve, delay))
      }

      return fulfillJson(route, turn)
    }

    // GET /state
    if (method === 'GET' && url.endsWith('/state')) {
      return fulfillJson(route, snapshot())
    }

    // GET /transcript
    if (method === 'GET' && url.endsWith('/transcript')) {
      return fulfillJson(route, transcript)
    }

    // GET /dag — return a canned DAG sized to currentTaskCount.
    if (method === 'GET' && url.endsWith('/dag')) {
      if (currentDag !== null) return fulfillJson(route, currentDag)
      if (currentTaskCount <= 0) return fulfillNotFound(route)
      const tasks: Record<string, unknown> = {}
      for (let i = 0; i < currentTaskCount; i += 1) {
        tasks[`task_${i}`] = {
          kind: 'computation',
          state: { status: 'pending' },
          depends_on: i > 0 ? [`task_${i - 1}`] : [],
          assignee: 'agent',
          description: `Mock task ${i}`,
          spec: null,
          resolution: null,
          result_ref: null,
        }
      }
      return fulfillJson(route, {
        version: '1',
        workflow_id: 'mock-workflow',
        current_task: null,
        tasks,
      })
    }

    // GET /metrics
    if (method === 'GET' && url.endsWith('/metrics')) {
      if (metrics === null) return fulfillNotFound(route)
      return fulfillJson(route, metrics)
    }

    // GET /proposals. Comes BEFORE the generic /confirm
    // / /reject session handlers so the URL pattern doesn't get
    // misrouted (e.g. the legacy `/reject` session-level confirmation
    // handler endsWith('/reject') matches our `.../proposal/PID/reject`
    // too).
    if (method === 'GET' && url.endsWith('/proposals')) {
      return fulfillJson(route, proposals)
    }

    // POST /proposal/:proposal_id/signoff
    if (method === 'POST' && /\/proposal\/[^/]+\/signoff$/.test(url)) {
      const match = url.match(/\/proposal\/([^/]+)\/signoff$/)
      const proposalId = match?.[1] ?? ''
      let body: unknown = null
      try {
        body = JSON.parse(route.request().postData() ?? 'null')
      } catch {
        body = null
      }
      recordedProposalActions.push({ verb: 'signoff', proposalId, body })
      return fulfillNoContent(route)
    }

    // POST /proposal/:proposal_id/reject
    if (method === 'POST' && /\/proposal\/[^/]+\/reject$/.test(url)) {
      const match = url.match(/\/proposal\/([^/]+)\/reject$/)
      const proposalId = match?.[1] ?? ''
      let body: unknown = null
      try {
        body = JSON.parse(route.request().postData() ?? 'null')
      } catch {
        body = null
      }
      recordedProposalActions.push({ verb: 'reject', proposalId, body })
      return fulfillNoContent(route)
    }

    // POST /confirm — session-level confirmation gate.
    if (method === 'POST' && url.endsWith('/confirm')) {
      userConfirmed = true
      currentState = confirmTarget
      inAfterConfirm = true
      afterConfirmIndex = 0
      // Broadcast state_advanced like the real server does.
      await pushSseEventToBrowser(page, sessionId, {
        type: 'state_advanced',
        new_state: currentState,
      })
      return fulfillNoContent(route)
    }

    // POST /reject — session-level rejection of the confirmation card.
    // Order matters: the /proposal/.../reject handler is registered
    // above so its URL match wins for proposal-scoped POSTs.
    if (method === 'POST' && url.endsWith('/reject')) {
      currentState = rejectTarget
      await pushSseEventToBrowser(page, sessionId, {
        type: 'state_advanced',
        new_state: currentState,
      })
      return fulfillNoContent(route)
    }

    // POST /unblock
    if (method === 'POST' && url.endsWith('/unblock')) {
      currentState = unblockTarget
      await pushSseEventToBrowser(page, sessionId, {
        type: 'state_advanced',
        new_state: currentState,
      })
      return fulfillNoContent(route)
    }

    // POST /progress — just accept, broadcast as harness_progress.
    if (method === 'POST' && url.endsWith('/progress')) {
      try {
        const payload = JSON.parse(route.request().postData() ?? '{}') as {
          kind: string
          task_id: string
          status: string
          detail: string
        }
        await pushSseEventToBrowser(page, sessionId, {
          type: 'harness_progress',
          kind: payload.kind,
          task_id: payload.task_id,
          status: payload.status,
          detail: payload.detail,
        })
      } catch {
        // ignore malformed
      }
      return fulfillNoContent(route)
    }

    // GET /events — the fake EventSource intercepts, but fall back just in case.
    if (method === 'GET' && url.endsWith('/events')) {
      return route.fulfill({
        status: 200,
        contentType: 'text/event-stream',
        body: '',
      })
    }

    return route.fallback()
  })

  // POST /api/chat/session/from-intent — deterministic fallback path
  // used when the chat assistant is disabled. It creates the same mock
  // session and records the structured prose as a user turn without
  // consuming a conversational beat.
  await routeChat(page, '/session/from-intent', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    let body: {
      goal?: string
      modality?: string
      organism?: string
      desired_outputs?: string
      uncertainties?: string
    }
    try {
      body = JSON.parse(route.request().postData() ?? '{}')
    } catch {
      return fulfillJson(route, { error: 'bad body' }, 400)
    }
    const lines = [
      body.goal ? `Goal: ${body.goal}` : null,
      body.modality ? `Modality: ${body.modality}` : null,
      body.organism ? `Organism: ${body.organism}` : null,
      body.desired_outputs ? `Desired outputs: ${body.desired_outputs}` : null,
      body.uncertainties ? `Uncertainties: ${body.uncertainties}` : null,
    ].filter((line): line is string => line !== null)
    transcript.push({
      turn_id: randomUuid(),
      role: 'user',
      content: lines.join('\n'),
      intent: null,
      tool_calls: [],
      quick_replies: [],
      confirmation_card: null,
      timestamp: new Date().toISOString(),
    })
    currentState = { kind: 'intake' }
    return fulfillJson(route, { session_id: sessionId, greeting })
  })

  const handle: MockBackendHandle = {
    sessionId,
    async pushSseEvent(event) {
      await pushSseEventToBrowser(page, sessionId, event)
    },
    async pushSseEvents(events) {
      for (const e of events) await pushSseEventToBrowser(page, sessionId, e)
    },
    recordedTurnMessages: () => [...recordedTurnMessages],
    turnCount: () => turnCallCount,
    setState(next) {
      currentState = next
    },
    setTaskCount(n) {
      currentTaskCount = n
    },
    enterAfterConfirmPhase() {
      inAfterConfirm = true
      afterConfirmIndex = 0
    },
    setMetrics(next) {
      metrics = next
    },
    failNextTurn(status) {
      forceNextTurnStatus = status
    },
    setProposals(next) {
      // Defensive clone so the test can still mutate its own copy
      // afterwards without aliasing the mock's internal state.
      proposals = [...next]
    },
    recordedProposalActions: () => [...recordedProposalActions],
    async dispose() {
      await unrouteChat(page, '/llm-availability')
      await unrouteChat(page, '/session/from-intent')
      await unrouteChat(page, '/session')
      await unrouteChat(page, '/session/*/**')
      await page.unroute('**/api/**').catch(() => {})
    },
  }

  return handle
}

async function pushSseEventToBrowser(
  page: Page,
  sessionId: string,
  event: SseEvent,
): Promise<void> {
  await page
    .evaluate(
      ({ sid, payload }) => {
        const push = (
          window as unknown as {
            __pushSseEvent?: (s: string, p: Record<string, unknown>) => void
          }
        ).__pushSseEvent
        if (push) push(sid, payload)
      },
      { sid: sessionId, payload: event as unknown as Record<string, unknown> },
    )
    .catch(() => {
      // Page may still be loading; drop the event rather than failing the test.
    })
}

function randomUuid(): string {
  // Simple v4-ish generator — we only need uniqueness within a test run.
  const hex = '0123456789abcdef'
  let out = ''
  for (let i = 0; i < 36; i += 1) {
    if (i === 8 || i === 13 || i === 18 || i === 23) out += '-'
    else if (i === 14) out += '4'
    else if (i === 19) out += hex[(Math.random() * 4) | (0x8 & 0xf)]
    else out += hex[(Math.random() * 16) | 0]
  }
  return out
}

// Helpers exported for scenario YAMLs.
export function asSessionState(kind: SessionStateKind): SessionState {
  return { kind } as SessionState
}
