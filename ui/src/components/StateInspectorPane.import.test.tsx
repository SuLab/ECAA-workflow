import { beforeEach, describe, expect, it, vi } from 'vitest'
import { render, screen } from '@testing-library/react'

// The pane pulls session + events state from context; mock both so we can
// drive `capabilities` directly and keep the render focused on the tab
// filter. Lazy tab bodies are stubbed so no dynamic import / fetch fires
// for the (default) Plan panel.
const h = vi.hoisted(() => ({
  session: null as unknown as Record<string, unknown>,
  events: null as unknown as Record<string, unknown>,
}))

vi.mock('../hooks/contexts', () => ({
  useSessionContext: () => h.session,
  useEventsContext: () => h.events,
}))

vi.mock('./state_inspector/lazy', () => {
  const Stub = () => null
  return {
    LazyCompareTab: Stub,
    LazyCompositionTab: Stub,
    LazyDashboardPane: Stub,
    LazyDecisionsTab: Stub,
    LazyFiguresPane: Stub,
    LazyHistoryPane: Stub,
    LazyInputsTab: Stub,
    LazyJobsFeed: Stub,
    LazyMetricsTable: Stub,
    LazyPlanTab: Stub,
    LazyRepairsTab: Stub,
    LazyStateTab: Stub,
  }
})

import StateInspectorPane from './StateInspectorPane'

const minimalCaps = {
  tier_label: 'minimal_audit' as const,
  explore: true,
  reverify: true,
  replay_tier1: true,
  replay_tier2: false,
  tabs: { composer_trace: false } as Record<string, boolean>,
}

function snapshot() {
  return {
    session_id: 's1',
    state: { kind: 'emitted' as const },
    user_confirmed: true,
    last_activity: '',
    task_count: 0,
    progress: { completed: 0, ready: 0, blocked: 0, pending: 0 },
    title: null,
    parent_session_id: null,
    blocked_tasks: [],
    pending_input_hints: [],
  }
}

function baseSession(capabilities: unknown) {
  return {
    sessionId: 's1',
    state: snapshot(),
    sending: false,
    dag: null,
    capabilities,
    markFresh: vi.fn(),
    markStale: vi.fn(),
  }
}

const baseEvents = {
  harnessProgress: [],
  pilot: null,
  stallSignals: {},
  crossVersionReport: null,
  executorInfo: null,
  heartbeatStalls: {},
  progressHealth: null,
  orphanReap: null,
}

beforeEach(() => {
  h.events = { ...baseEvents }
  // Effects (compose-outcome / metrics / pilot) funnel through fetch; reject
  // so their try/catch swallow it without real network.
  vi.stubGlobal(
    'fetch',
    vi.fn(() => Promise.reject(new Error('no network in test'))),
  )
})

describe('StateInspectorPane — imported package tab gating', () => {
  it('hides Composition / Performance / Compare / Composer trace for an imported minimal package', () => {
    h.session = baseSession({ imported: true, capabilities: minimalCaps })
    render(<StateInspectorPane />)

    // Plan stays available.
    expect(screen.getByRole('tab', { name: 'Plan' })).toBeInTheDocument()
    // Degraded / verifier-less-backed tabs are hidden.
    expect(screen.queryByRole('tab', { name: 'Composition' })).toBeNull()
    expect(screen.queryByRole('tab', { name: 'Performance' })).toBeNull()
    expect(screen.queryByRole('tab', { name: 'Compare' })).toBeNull()
    // composer_trace flag is false → Composer trace hidden.
    expect(screen.queryByRole('tab', { name: 'Composer trace' })).toBeNull()
  })

  it('keeps Composition + Composer trace for an ordinary (non-imported) session', () => {
    h.session = baseSession(null)
    render(<StateInspectorPane />)

    expect(screen.getByRole('tab', { name: 'Composition' })).toBeInTheDocument()
    expect(
      screen.getByRole('tab', { name: 'Composer trace' }),
    ).toBeInTheDocument()
  })

  it('shows Composer trace for an imported package whose probe flags composer_trace true', () => {
    h.session = baseSession({
      imported: true,
      capabilities: { ...minimalCaps, tabs: { composer_trace: true } },
    })
    render(<StateInspectorPane />)

    expect(
      screen.getByRole('tab', { name: 'Composer trace' }),
    ).toBeInTheDocument()
    // Composition still hidden regardless of the composer_trace flag.
    expect(screen.queryByRole('tab', { name: 'Composition' })).toBeNull()
  })
})
