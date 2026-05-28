// Test wrapper that mounts `SessionProvider` + `EventsProvider` around a
// child tree so components reaching `useSessionContext()` /
// `useEventsContext()` outside `App.tsx` (i.e. in jsdom unit tests) get
// a real context value instead of throwing the "wrap the tree in
// App.tsx" guard.
//
// Production code still owns the hook lifecycle in App.tsx; this helper
// just supplies a stub value with the same shape so isolated component
// renders work in Vitest.

import type { ReactNode } from 'react'
import { vi } from 'vitest'
import { EventsProvider, SessionProvider } from '../hooks/contexts'
import { StreamingTextProvider } from '../state/StreamingTextContext'
import type { useConversation } from '../hooks/useConversation'
import type { useSseChatEvents } from '../hooks/useSseChatEvents'

type SessionValue = ReturnType<typeof useConversation>
type EventsValue = ReturnType<typeof useSseChatEvents>

export function makeMockSessionValue(
  overrides: Partial<SessionValue> = {},
): SessionValue {
  const base: SessionValue = {
    sessionId: 'test-session-id',
    turns: [],
    state: null,
    dag: null,
    sending: false,
    stillThinking: false,
    thinkingStage: 'idle',
    cancelTurn: vi.fn(),
    error: null,
    staleSources: new Set<string>(),
    start: vi.fn().mockResolvedValue(undefined),
    sendTurn: vi.fn().mockResolvedValue(undefined),
    confirm: vi.fn().mockResolvedValue(undefined),
    reject: vi.fn().mockResolvedValue(undefined),
    unblock: vi.fn().mockResolvedValue(undefined),
    reset: vi.fn().mockResolvedValue(undefined),
    switchToSession: vi.fn().mockResolvedValue(undefined),
    refreshCurrentState: vi.fn().mockResolvedValue(undefined),
    applyStateAdvanced: vi.fn().mockResolvedValue(undefined),
    refreshDag: vi.fn().mockResolvedValue(undefined),
    resyncAll: vi.fn().mockResolvedValue(undefined),
    appendTurn: vi.fn(),
    markStale: vi.fn(),
    markFresh: vi.fn(),
    executionRunning: false,
    startExecutionAction: vi.fn().mockResolvedValue(undefined),
  }
  return { ...base, ...overrides }
}

export function makeMockEventsValue(
  overrides: Partial<EventsValue> = {},
): EventsValue {
  const base: EventsValue = {
    toolCallPill: null,
    infraError: null,
    harnessProgress: [],
    harnessProgressDropped: 0,
    reviewableTasks: new Set<string>(),
    reviewableArtifacts: {},
    pilot: { status: null, report: null, skipReason: null },
    stallSignals: {},
    crossVersionReport: null,
    executorInfo: null,
    progressHealth: null,
    orphanReap: null,
    heartbeatStalls: {},
    proposalEvents: {},
    clearInfraError: vi.fn(),
  }
  return { ...base, ...overrides }
}

export function SessionTestWrapper({
  session,
  events,
  children,
}: {
  session?: Partial<SessionValue>
  events?: Partial<EventsValue>
  children: ReactNode
}): JSX.Element {
  return (
    <SessionProvider value={makeMockSessionValue(session)}>
      <EventsProvider value={makeMockEventsValue(events)}>
        <StreamingTextProvider>{children}</StreamingTextProvider>
      </EventsProvider>
    </SessionProvider>
  )
}
