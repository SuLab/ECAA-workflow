// Automated accessibility audit. Renders each top-level chat component
// into jsdom and runs axe-core against the DOM; asserts zero WCAG AA
// violations. axe-core is used directly (not jest-axe) because
// @testing-library/react + axe-core works without an extra wrapper.

import { describe, expect, it, vi } from 'vitest'
import { render } from '@testing-library/react'
import { ReactFlowProvider } from '@xyflow/react'
import axe from 'axe-core'

import AssistantTurnCard from '../components/AssistantTurnCard'
import BlockerCard from '../components/BlockerCard'
import BranchFromHereCard from '../components/BranchFromHereCard'
import ChatComposer from '../components/ChatComposer'
import ConfirmationTurnCard from '../components/ConfirmationTurnCard'
import ErrorBoundary from '../components/ErrorBoundary'
import InfraErrorBanner from '../components/InfraErrorBanner'
import QuickReplyRow from '../components/QuickReplyRow'
import SensitivityComparisonCard from '../components/SensitivityComparisonCard'
import StillThinkingIndicator from '../components/StillThinkingIndicator'
import StructuredCaptureTurnCard from '../components/StructuredCaptureTurnCard'
import TaskCard from '../components/TaskCard'
import ToolCallStatusPill from '../components/ToolCallStatusPill'
import UserTurnCard from '../components/UserTurnCard'
import TaskLogDrawer from '../components/TaskLogDrawer'
import { DashboardPane } from '../components/state_inspector/DashboardTab'
import { FiguresPane } from '../components/state_inspector/FiguresTab'
import { PlaceholderPane } from '../components/state_inspector/common'
import { StateTab } from '../components/state_inspector/StateTab'
import { DocumentsPane } from '../components/state_inspector/DocumentsTab'
import { JobsFeed } from '../components/state_inspector/JobsTab'
import type { Task, Turn } from '../types'
import { SessionTestWrapper } from './sessionTestHelpers'

async function runAxe(node: HTMLElement) {
  // axe-core needs a real-ish browser API surface; jsdom provides it.
  // We run with the WCAG 2.1 AA rule set which matches what the
  // accessibility audit doc commits to.
  //
  // Color-contrast stays disabled in this jsdom run
  // because jsdom doesn't implement canvas getContext(); axe-core
  // falls back to a noisy stderr warning rather than a real check.
  // The static `colorContrast.test.tsx` companion runs the WCAG 2.1
  // contrast formula directly against `tokens.ts` for every load-
  // bearing fg/bg pair (textPrimary on surface0, accentFg on accent,
  // borderStrong on surface0, etc.) and asserts AA — covering the
  // rule out of band rather than ignoring it.
  const results = await axe.run(node, {
    runOnly: {
      type: 'tag',
      values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'],
    },
    rules: {
      'color-contrast': { enabled: false },
    },
  })
  return results
}

function makeAssistantTurn(content: string): Turn {
  return {
    turn_id: 'test-turn-1',
    role: 'assistant',
    content,
    intent: null,
    tool_calls: [],
    quick_replies: [],
    confirmation_card: null,
    timestamp: new Date().toISOString(),
  }
}

function makeUserTurn(content: string): Turn {
  return {
    turn_id: 'test-turn-2',
    role: 'user',
    content,
    intent: null,
    tool_calls: [],
    quick_replies: [],
    confirmation_card: null,
    timestamp: new Date().toISOString(),
  }
}

describe('a11y (axe-core) — natural-chat components', () => {
  it('ToolCallStatusPill has no axe violations', async () => {
    const { container } = render(
      <ToolCallStatusPill statusLine="Checking the plan against your description…" />,
    )
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })

  it('StillThinkingIndicator has no axe violations', async () => {
    const { container } = render(<StillThinkingIndicator />)
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })

  it('BlockerCard has no axe violations', async () => {
    const { container } = render(
      <BlockerCard
        reason="anthropic API unreachable"
        recoveryHint="Wait for the underlying service to recover and try again."
        onUnblock={vi.fn()}
      />,
    )
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })

  it('InfraErrorBanner has no axe violations', async () => {
    const { container } = render(
      <InfraErrorBanner
        error={{ reason: 'api_unreachable', userCopy: 'Try again.' }}
        onDismiss={vi.fn()}
      />,
    )
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })

  it('ChatComposer has no axe violations', async () => {
    const { container } = render(<ChatComposer onSend={vi.fn()} autoFocus={false} />)
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })

  it('AssistantTurnCard has no axe violations', async () => {
    const { container } = render(
      <AssistantTurnCard
        turn={makeAssistantTurn('Got it — looking up the plan.')}
        isLatest={false}
        pillStatusLine={null}
        onConfirm={vi.fn()}
        onReject={vi.fn()}
        onQuickReply={vi.fn()}
      />,
    )
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })

  it('UserTurnCard has no axe violations', async () => {
    const { container } = render(<UserTurnCard turn={makeUserTurn('hello')} />)
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })

  it('ConfirmationTurnCard has no axe violations', async () => {
    const { container } = render(
      <SessionTestWrapper>
        <ConfirmationTurnCard
          card={{ summary_markdown: 'Plan: 47 IVD libraries, degenerated vs healthy.', summary_hash: 'a'.repeat(64) }}
          onConfirm={vi.fn()}
          onReject={vi.fn()}
        />
      </SessionTestWrapper>,
    )
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })

  it('QuickReplyRow has no axe violations', async () => {
    const { container } = render(
      <QuickReplyRow
        options={['Bulk RNA-seq', 'Single-cell RNA-seq', 'Something else']}
        onPick={vi.fn()}
      />,
    )
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })

  it('StructuredCaptureTurnCard has no axe violations', async () => {
    const { container } = render(
      <StructuredCaptureTurnCard
        title="Per-study accession metadata"
        description="Add one row per study so the agent can stratify."
        fields={[
          { key: 'gse', label: 'GEO accession', required: true },
          { key: 'n', label: 'Sample count', required: true },
          { key: 'tissue', label: 'Tissue', placeholder: 'e.g. ileal biopsy' },
        ]}
        onSubmit={vi.fn()}
      />,
    )
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })

  it('TaskLogDrawer has no axe violations', async () => {
    (globalThis as unknown as { fetch: typeof fetch }).fetch = vi
      .fn()
      .mockResolvedValue(new Response('', { status: 404 })) as unknown as typeof fetch
    const { container } = render(
      <TaskLogDrawer sessionId="s1" taskId="t1" onClose={() => {}} />,
    )
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })

  it('DashboardPane empty state has no axe violations', async () => {
    (globalThis as unknown as { fetch: typeof fetch }).fetch = vi
      .fn()
      .mockResolvedValue(
        new Response(JSON.stringify({ session_id: 's1', stages: [] }), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        }),
      ) as unknown as typeof fetch
    const { container, findByTestId } = render(<DashboardPane sessionId="s1" />)
    await findByTestId('state-dashboard-pane')
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })

  it('FiguresPane empty state has no axe violations', async () => {
    const { container } = render(<FiguresPane sessionId={null} dag={null} />)
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })

  it('BranchFromHereCard has no axe violations', async () => {
    const { container } = render(<BranchFromHereCard onBranch={vi.fn()} />)
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })

  it('SensitivityComparisonCard methods mode has no axe violations', async () => {
    const { container } = render(
      <SensitivityComparisonCard
        stage="normalization"
        candidates={['log-cpm', 'sctransform']}
        onSelect={vi.fn()}
      />,
    )
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })

  it('ErrorBoundary fallback alert has no axe violations', async () => {
    const consoleError = vi
      .spyOn(console, 'error')
      .mockImplementation(() => {})
    const Bomb = () => {
      throw new Error('boom')
    }
    const { container } = render(
      <ErrorBoundary fallbackLabel="the chat">
        <Bomb />
      </ErrorBoundary>,
    )
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
    consoleError.mockRestore()
  })

  it('PlaceholderPane has no axe violations', async () => {
    const { container } = render(
      <PlaceholderPane>Nothing to display yet.</PlaceholderPane>,
    )
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })

  it('StateTab has no axe violations', async () => {
    const { container } = render(<StateTab state={null} />)
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })

  it('DocumentsPane has no axe violations', async () => {
    const { container } = render(<DocumentsPane state={null} sessionId={null} />)
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })

  it('JobsFeed empty state has no axe violations', async () => {
    const { container } = render(<JobsFeed events={[]} />)
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })

  it('TaskCard exposes a focusable button with onActivate', async () => {
    const onActivate = vi.fn()
    const task: Task = {
      kind: 'computation',
      depends_on: [],
      description: 'preprocess',
      spec: {},
      state: { status: 'ready' },
    } as unknown as Task
    // ReactFlow's NodeProps shape is internal; cast via unknown to keep
    // the test independent of @xyflow's exact prop surface.
    const props = {
      id: 't1',
      type: 'taskCard',
      data: { task, active: false, dim: false, onActivate },
      selected: false,
      zIndex: 0,
      isConnectable: true,
      dragging: false,
    } as unknown as Parameters<typeof TaskCard>[0]
    const { container, getByRole } = render(
      <ReactFlowProvider>
        <TaskCard {...props} />
      </ReactFlowProvider>,
    )
    const btn = getByRole('button')
    expect(btn).toHaveAttribute('tabindex', '0')
    btn.focus()
    expect(document.activeElement).toBe(btn)
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })
})
