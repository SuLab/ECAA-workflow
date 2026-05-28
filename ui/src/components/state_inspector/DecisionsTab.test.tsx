// Coverage: stage→task resolution (PR-F P1-7 fix), filter chips,
// "Open step" hidden when no resolvable target, headline rendering
// across decision kinds.
//
// The decision list virtualizes through react-virtuoso (C24), which
// needs `ResizeObserver` + a non-zero viewport. jsdom omits both; we
// stub them here so Virtuoso mounts every row in these small fixtures.

import { fireEvent, render, waitFor } from '@testing-library/react'
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest'

import { DecisionsTab } from './DecisionsTab'
import type { DAG } from '../../types'
import * as chatClient from '../../api/chatClient'

beforeAll(() => {
  if (typeof window !== 'undefined' && !('ResizeObserver' in window)) {
    class ResizeObserverStub {
      observe(): void {}
      unobserve(): void {}
      disconnect(): void {}
    }
    Object.defineProperty(window, 'ResizeObserver', {
      writable: true,
      configurable: true,
      value: ResizeObserverStub,
    })
  }
  const originalGetBoundingClientRect =
    HTMLElement.prototype.getBoundingClientRect
  HTMLElement.prototype.getBoundingClientRect = function (): DOMRect {
    const r = originalGetBoundingClientRect.call(this) as DOMRect
    return new DOMRect(0, 0, r.width || 1024, r.height || 800)
  }
})

const fakeDag = {
  tasks: {
    'normalize-1': {
      kind: 'computation',
      depends_on: [],
      description: 'normalize',
      spec: { stage_class: 'normalization' },
      state: { status: 'completed' },
    } as unknown,
    'unrelated-2': {
      kind: 'computation',
      depends_on: [],
      description: 'unrelated',
      spec: { stage_class: 'something_else' },
      state: { status: 'pending' },
    } as unknown,
  },
} as unknown as DAG

beforeEach(() => {
  vi.spyOn(chatClient, 'getDecisions').mockResolvedValue({
    session_id: 's1',
    decisions: [
      {
        timestamp: '2026-04-27T10:00:00Z',
        session_id: 's1',
        decision: { kind: 'amend_stage', stage: 'normalization', method_prose: 'log-cpm' },
        actor: 'sme',
      },
      {
        timestamp: '2026-04-27T10:05:00Z',
        session_id: 's1',
        decision: { kind: 'confirm' },
        actor: 'sme',
      },
      {
        timestamp: '2026-04-27T10:10:00Z',
        session_id: 's1',
        decision: { kind: 'amend_stage', stage: 'orphan_stage', method_prose: 'foo' },
        actor: 'sme',
      },
    ],
  })
})

afterEach(() => {
  vi.restoreAllMocks()
})

describe('DecisionsTab', () => {
  it('renders the decision list once getDecisions resolves', async () => {
    const { findAllByText, findByText } = render(
      <DecisionsTab sessionId="s1" dag={fakeDag} />,
    )
    expect((await findAllByText(/changed the method/i)).length).toBeGreaterThan(0)
    expect(await findByText(/confirmed the plan/i)).toBeInTheDocument()
  })

  it('"Open step" calls onJumpToTask with the resolved task id when stage matches a DAG task', async () => {
    const onJumpToTask = vi.fn()
    const { findAllByText } = render(
      <DecisionsTab
        sessionId="s1"
        dag={fakeDag}
        onJumpToTask={onJumpToTask}
      />,
    )
    // Wait until at least one decision card has an "Open step" button.
    const buttons = await waitFor(() => findAllByText(/Open step/i))
    fireEvent.click(buttons[0]!)
    expect(onJumpToTask).toHaveBeenCalled()
    // The amend_stage on `normalization` resolves to `normalize-1`.
    expect(onJumpToTask.mock.calls.some((c) => c[0] === 'normalize-1')).toBe(
      true,
    )
  })

  it('hides "Open step" when stage does not resolve to any task', async () => {
    const onJumpToTask = vi.fn()
    const { findAllByText, container } = render(
      <DecisionsTab
        sessionId="s1"
        dag={fakeDag}
        onJumpToTask={onJumpToTask}
      />,
    )
    await findAllByText(/changed the method/i)
    // Three decisions in the fixture; only one resolves (normalization).
    // The confirm + the orphan_stage amend should NOT render a button.
    const buttons = container.querySelectorAll('button')
    const openStepButtons = Array.from(buttons).filter((b) =>
      b.textContent?.includes('Open step'),
    )
    expect(openStepButtons).toHaveLength(1)
  })

  it('filters by kind when a chip is clicked', async () => {
    const { findByRole } = render(<DecisionsTab sessionId="s1" dag={fakeDag} />)
    fireEvent.click(await findByRole('button', { name: /Confirmations/i }))
    await waitFor(() => {
      expect(chatClient.getDecisions).toHaveBeenLastCalledWith('s1', 'confirm')
    })
  })

  it('renders adapter_decision_recorded summary', async () => {
    vi.restoreAllMocks()
    vi.spyOn(chatClient, 'getDecisions').mockResolvedValue({
      session_id: 's1',
      decisions: [
        {
          timestamp: '2026-04-27T10:00:00Z',
          session_id: 's1',
          decision: {
            kind: 'adapter_decision_recorded',
            adapter_id: 'adapter-7',
            decision: 'confirmed',
            safety: 'lossy_declared',
          },
          actor: 'sme',
        },
      ],
    })
    const { findByText } = render(<DecisionsTab sessionId="s1" />)
    expect(
      await findByText(/confirmed adapter adapter-7 \(lossy_declared\)/i),
    ).toBeInTheDocument()
  })

  it('renders novel_node_decision_recorded summary', async () => {
    vi.restoreAllMocks()
    vi.spyOn(chatClient, 'getDecisions').mockResolvedValue({
      session_id: 's1',
      decisions: [
        {
          timestamp: '2026-04-27T10:00:00Z',
          session_id: 's1',
          decision: {
            kind: 'novel_node_decision_recorded',
            node_id: 'hyp-node-3',
            decision: 'accepted_as_draft',
          },
          actor: 'sme',
        },
      ],
    })
    const { findByText } = render(<DecisionsTab sessionId="s1" />)
    expect(
      await findByText(/accepted_as_draft novel node hyp-node-3/i),
    ).toBeInTheDocument()
  })

  it('renders refusal_acknowledged summary', async () => {
    vi.restoreAllMocks()
    vi.spyOn(chatClient, 'getDecisions').mockResolvedValue({
      session_id: 's1',
      decisions: [
        {
          timestamp: '2026-04-27T10:00:00Z',
          session_id: 's1',
          decision: {
            kind: 'refusal_acknowledged',
            refusal_id: 'refusal-42',
            recovery: 'branch',
          },
          actor: 'sme',
        },
      ],
    })
    const { findByText } = render(<DecisionsTab sessionId="s1" />)
    expect(
      await findByText(/acknowledged refusal refusal-42 via branch/i),
    ).toBeInTheDocument()
  })

  it('renders assumption_contradicted summary', async () => {
    vi.restoreAllMocks()
    vi.spyOn(chatClient, 'getDecisions').mockResolvedValue({
      session_id: 's1',
      decisions: [
        {
          timestamp: '2026-04-27T10:00:00Z',
          session_id: 's1',
          decision: {
            kind: 'assumption_contradicted',
            assumption_id: 'a_1',
            prior_confirmation_id: 'conf-prior',
            conflicting_confirmation_id: 'conf-new',
          },
          actor: 'sme',
        },
      ],
    })
    const { findByText } = render(<DecisionsTab sessionId="s1" />)
    expect(
      await findByText(
        /contradicted assumption a_1 \(prior conf-prior vs conf-new\)/i,
      ),
    ).toBeInTheDocument()
  })

  it('renders assumption_waived summary', async () => {
    vi.restoreAllMocks()
    vi.spyOn(chatClient, 'getDecisions').mockResolvedValue({
      session_id: 's1',
      decisions: [
        {
          timestamp: '2026-04-27T10:00:00Z',
          session_id: 's1',
          decision: {
            kind: 'assumption_waived',
            assumption_id: 'a_2',
            policy_rule_id: 'rule-batch-correction-skip',
            rationale: 'pilot run',
            credentials: ['lead-pi'],
          },
          actor: 'sme',
        },
      ],
    })
    const { findByText } = render(<DecisionsTab sessionId="s1" />)
    expect(
      await findByText(
        /waived assumption a_2 under policy rule-batch-correction-skip/i,
      ),
    ).toBeInTheDocument()
  })

  it('renders assumption_invalidated summary', async () => {
    vi.restoreAllMocks()
    vi.spyOn(chatClient, 'getDecisions').mockResolvedValue({
      session_id: 's1',
      decisions: [
        {
          timestamp: '2026-04-27T10:00:00Z',
          session_id: 's1',
          decision: {
            kind: 'assumption_invalidated',
            assumption_id: 'a_3',
            upstream_change: 'normalization method change',
          },
          actor: 'harness',
        },
      ],
    })
    const { findByText } = render(<DecisionsTab sessionId="s1" />)
    expect(
      await findByText(
        /invalidated assumption a_3 due to normalization method change/i,
      ),
    ).toBeInTheDocument()
  })

  it('renders lifecycle_transition summary', async () => {
    vi.restoreAllMocks()
    vi.spyOn(chatClient, 'getDecisions').mockResolvedValue({
      session_id: 's1',
      decisions: [
        {
          timestamp: '2026-04-27T10:00:00Z',
          session_id: 's1',
          decision: {
            kind: 'lifecycle_transition',
            transition_kind: 'promotion',
            payload: '{"from":"draft","to":"published"}',
          },
          actor: 'harness',
        },
      ],
    })
    const { findByText } = render(<DecisionsTab sessionId="s1" />)
    expect(
      await findByText(/recorded a lifecycle promotion transition/i),
    ).toBeInTheDocument()
  })

  it('renders contradiction_detected summary', async () => {
    vi.restoreAllMocks()
    vi.spyOn(chatClient, 'getDecisions').mockResolvedValue({
      session_id: 's1',
      decisions: [
        {
          timestamp: '2026-04-27T10:00:00Z',
          session_id: 's1',
          decision: {
            kind: 'contradiction_detected',
            assumption_id: 'a_4',
            prior_id: 'p-1',
            new_id: 'n-1',
          },
          actor: 'harness',
        },
      ],
    })
    const { findByText } = render(<DecisionsTab sessionId="s1" />)
    expect(
      await findByText(
        /detected contradiction on assumption a_4 \(prior p-1 vs new n-1\)/i,
      ),
    ).toBeInTheDocument()
  })

  it('renders invalidation_cascaded summary', async () => {
    vi.restoreAllMocks()
    vi.spyOn(chatClient, 'getDecisions').mockResolvedValue({
      session_id: 's1',
      decisions: [
        {
          timestamp: '2026-04-27T10:00:00Z',
          session_id: 's1',
          decision: {
            kind: 'invalidation_cascaded',
            assumption_id: 'a_5',
            affected: ['task-1', 'task-2', 'task-3'],
          },
          actor: 'harness',
        },
      ],
    })
    const { findByText } = render(<DecisionsTab sessionId="s1" />)
    expect(
      await findByText(
        /cascaded invalidation of assumption a_5 to 3 downstream entries/i,
      ),
    ).toBeInTheDocument()
  })

  it('drops malformed records (agent free-form audit entries) before render', async () => {
    // Regression: the agent (claude subprocess) sometimes appends
    // free-form entries to runtime/decisions.jsonl that don't match
    // the typed DecisionRecord shape — `kind` lives at the top level
    // instead of nested under `decision.kind`. Without filtering,
    // DecisionCard's `record.decision.kind` access crashes the tab
    // ("can't access property kind, decision is undefined").
    vi.restoreAllMocks()
    vi.spyOn(chatClient, 'getDecisions').mockResolvedValue({
      session_id: 's1',
      decisions: [
        {
          timestamp: '2026-04-27T10:00:00Z',
          session_id: 's1',
          decision: { kind: 'confirm' },
          actor: 'sme',
        },
        // Agent-appended free-form audit entry — `kind` at top level,
        // no `.decision` nesting. Must be dropped silently.
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        {
          timestamp: '2026-04-27T10:01:00Z',
          task_id: 'discover_preprocessing',
          kind: 'discovery_auto_pick',
          stage_class: 'preprocessing_qc',
          top_candidate: 'sctransform_v2',
          auto_picked: true,
        } as any,
        // Also-malformed: explicit decision: null.
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        {
          timestamp: '2026-04-27T10:02:00Z',
          session_id: 's1',
          decision: null,
          actor: 'llm',
        } as any,
      ],
    })
    const { findByText, queryByText, container } = render(
      <DecisionsTab sessionId="s1" dag={fakeDag} />,
    )
    // The valid record renders.
    expect(await findByText(/confirmed the plan/i)).toBeInTheDocument()
    // No crash trying to render a card for the agent-stamped entry.
    expect(queryByText(/discovery_auto_pick/i)).toBeNull()
    // Only one card list item.
    expect(container.querySelectorAll('li').length).toBe(1)
  })

  // Install-log surface lives next to the
  // decision list so the SME has both "what changed" and "what was
  // installed" in one place.
  describe('install-log section', () => {
    it('renders install-log rows when the endpoint returns entries', async () => {
      vi.spyOn(chatClient, 'getInstallLog').mockResolvedValue({
        entries: [
          {
            atom_id: 'rnaseq_align',
            package: 'samtools',
            registry: 'apt',
            timestamp: 1700000000.0,
          },
          {
            atom_id: 'rnaseq_align',
            package: 'pandas',
            registry: 'pip',
            timestamp: 1700000010.5,
          },
        ],
      })
      const { findAllByTestId, findByText } = render(
        <DecisionsTab sessionId="s1" dag={fakeDag} />,
      )
      // Header indicates the install log is present.
      expect(await findByText(/runtime install log/i)).toBeInTheDocument()
      const rows = await findAllByTestId('install-log-row')
      expect(rows).toHaveLength(2)
      // First row carries the canonical fields.
      expect(rows![0]!.textContent).toContain('rnaseq_align')
      expect(rows![0]!.textContent).toContain('samtools')
      expect(rows![0]!.textContent).toContain('apt')
      // Second row keeps the row order from the JSONL.
      expect(rows![1]!.textContent).toContain('pip')
    })

    it('hides the install-log section when the endpoint returns empty entries', async () => {
      vi.spyOn(chatClient, 'getInstallLog').mockResolvedValue({
        entries: [],
      })
      const { findAllByText, queryByText } = render(
        <DecisionsTab sessionId="s1" dag={fakeDag} />,
      )
      // Decisions still render (two of the three fixture rows match
      // "changed the method...") — install-log section is suppressed.
      expect((await findAllByText(/changed the method/i)).length).toBeGreaterThan(0)
      expect(queryByText(/runtime install log/i)).toBeNull()
    })
  })
})
