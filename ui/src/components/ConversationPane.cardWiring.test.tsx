/**
 * Card-wiring regression: the inline `SensitivityComparisonCard` and
 * `ResultReviewTurnCard` must drive the DETERMINISTIC REST endpoints
 * (`postSmeSelection` / `postRerun`) rather than threading the SME's
 * intent through the LLM via `conv.sendTurn`.
 *
 * The deterministic endpoints are gated server-side and emit their
 * typed `DecisionRecord` (`SelectSensitivityWinner` / `RerunTask`) at
 * mutation time — routing through the LLM both adds a round-trip and
 * loses the at-mutation audit record. `sendTurn` is retained only as a
 * fallback (no session id, or REST failure), which these tests assert
 * is NOT taken on the happy path.
 *
 * ConversationPane owns the callbacks (`onSelectSensitivityWinner`,
 * `onRerunTask`) and consumes `conv`/`sse` via context, so the wiring
 * is exercised by mounting the full pane with stub providers + a mocked
 * chatClient.
 */
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi, beforeEach } from 'vitest'

import ConversationPane from './ConversationPane'
import { SessionProvider, EventsProvider } from '../hooks/contexts'
import { StreamingTextProvider } from '../state/StreamingTextContext'
import * as chatClient from '../api/chatClient'
import type { SessionStateSnapshot } from '../api/chatClient'

// Stub the mount-time network calls so no real fetch fires in jsdom, and
// expose the two REST helpers under test as spies. Everything else
// (handlers fired only by user action that we don't trigger) stays real.
vi.mock('../api/chatClient', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../api/chatClient')>()
  return {
    ...actual,
    getLlmAvailability: vi.fn().mockResolvedValue({ kind: 'available' }),
    listDispositions: vi.fn().mockResolvedValue({ dispositions: [] }),
    getProposals: vi.fn().mockResolvedValue([]),
    getTaskResult: vi.fn().mockResolvedValue(null),
    // BlockerCard also renders for the awaiting_sme_selection blocker and
    // polls these on mount — stub so no real fetch fires in jsdom.
    getTaskBlocker: vi.fn().mockResolvedValue(null),
    getTaskBlockerPayload: vi
      .fn()
      .mockResolvedValue({ blocker: null, attempts: [] }),
    postSmeSelection: vi.fn().mockResolvedValue(undefined),
    postRerun: vi
      .fn()
      .mockResolvedValue({ task_id: 't1', invalidated_tasks: [] }),
  }
})

const SESSION_ID = 'sess-1'

function blockedSnapshot(
  stageId: string,
  candidates: string[],
): SessionStateSnapshot {
  return {
    session_id: SESSION_ID,
    state: {
      kind: 'blocked',
      blockers: [
        {
          blocker_id: 'b-1',
          task_id: stageId,
          kind: {
            kind: 'awaiting_sme_selection',
            stage_id: stageId,
            candidates,
          },
          message: 'Pick a winner',
          recovery_hint: undefined,
          at: '2026-05-31T00:00:00Z',
        },
      ],
      reason: 'Pick a winner',
      recovery_hint: '',
    },
    user_confirmed: true,
    last_activity: '2026-05-31T00:00:00Z',
    task_count: 1,
    progress: {} as SessionStateSnapshot['progress'],
    emitted_package_path: '/tmp/pkg',
    title: null,
    parent_session_id: null,
    blocked_tasks: [stageId],
    pending_input_hints: [],
  }
}

// Minimal `conv` (useConversation return) stub. Only the fields
// ConversationPane reads on the sensitivity path need real values; the
// rest are inert stubs so the component renders without throwing.
function makeConv(
  overrides: Partial<ReturnType<typeof import('../hooks/useConversation').useConversation>>,
) {
  const base = {
    sessionId: SESSION_ID,
    turns: [],
    state: null,
    dag: null,
    sending: false,
    stillThinking: false,
    thinkingStage: null,
    cancelTurn: vi.fn(),
    error: null,
    staleSources: new Set<string>(),
    start: vi.fn(),
    sendTurn: vi.fn().mockResolvedValue(undefined),
    confirm: vi.fn(),
    reject: vi.fn(),
    unblock: vi.fn(),
    reset: vi.fn(),
    switchToSession: vi.fn(),
    refreshCurrentState: vi.fn(),
    applyStateAdvanced: vi.fn(),
    refreshDag: vi.fn(),
    resyncAll: vi.fn(),
    appendTurn: vi.fn(),
    markStale: vi.fn(),
    markFresh: vi.fn(),
    executionRunning: false,
    startExecutionAction: vi.fn(),
  }
  return { ...base, ...overrides } as ReturnType<
    typeof import('../hooks/useConversation').useConversation
  >
}

// Minimal `sse` (useSseChatEvents return) stub — empty event surfaces.
function makeSse() {
  return {
    toolCallPill: null,
    infraError: null,
    harnessProgress: [],
    harnessProgressDropped: 0,
    reviewableTasks: new Set<string>(),
    reviewableArtifacts: {},
    pilot: null,
    stallSignals: [],
    crossVersionReport: null,
    executorInfo: null,
    progressHealth: null,
    orphanReap: null,
    heartbeatStalls: [],
    proposalEvents: {},
    clearInfraError: vi.fn(),
  } as unknown as ReturnType<
    typeof import('../hooks/useSseChatEvents').useSseChatEvents
  >
}

function renderPane(
  conv: ReturnType<typeof makeConv>,
  sse: ReturnType<typeof makeSse>,
) {
  return render(
    <StreamingTextProvider>
      <SessionProvider value={conv}>
        <EventsProvider value={sse}>
          <ConversationPane />
        </EventsProvider>
      </SessionProvider>
    </StreamingTextProvider>,
  )
}

describe('ConversationPane card wiring → deterministic REST', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  it('onSelectSensitivityWinner posts to /sme-selection, not sendTurn', async () => {
    const user = userEvent.setup()
    const sendTurn = vi.fn().mockResolvedValue(undefined)
    const conv = makeConv({
      sendTurn,
      state: blockedSnapshot('discover_integration', ['scVI', 'Harmony', 'CCA']),
    })
    renderPane(conv, makeSse())

    // The SensitivityComparisonCard renders off the awaiting_sme_selection
    // blocker. Pick a winner + submit.
    await user.click(await screen.findByRole('radio', { name: /Select scVI/i }))
    await user.click(screen.getByRole('button', { name: /Record choice/i }))

    await waitFor(() => {
      expect(chatClient.postSmeSelection).toHaveBeenCalledWith(
        SESSION_ID,
        'discover_integration',
        'scVI',
      )
    })
    expect(sendTurn).not.toHaveBeenCalled()
  })
})
