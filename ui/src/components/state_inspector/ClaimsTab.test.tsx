import { describe, expect, it, beforeEach, vi } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'

import type { DAG } from '../../types/DAG'
import type { ClaimVerificationReport } from '../../types/ClaimVerificationReport'
import { getTaskResult, verifyTask } from '../../api/chatClient'
import { ClaimsTab } from './ClaimsTab'

vi.mock('../../api/chatClient', () => ({
  getTaskResult: vi.fn(),
  verifyTask: vi.fn(),
}))

const mockGetTaskResult = vi.mocked(getTaskResult)
const mockVerifyTask = vi.mocked(verifyTask)

function completedDag(taskId: string): DAG {
  return {
    version: '1',
    workflow_id: 'wf-test',
    current_task: null,
    tasks: {
      [taskId]: {
        state: { status: 'completed' },
      },
    },
  } as unknown as DAG
}

function report(
  status: 'verified' | 'mismatch' | 'unverifiable',
): ClaimVerificationReport {
  const claim = {
    entity: 'ACAN',
    direction: 'up' as const,
    effect_size: 2.1,
    pvalue: 0.001,
    source_table: 'Table S1',
    excerpt: 'ACAN was upregulated (log2FC=2.1, padj=0.001, Table S1).',
    contract: 'numeric_table_lookup' as const,
  }
  const verdictStatus =
    status === 'verified'
      ? { status }
      : status === 'mismatch'
        ? {
            status,
            detail: 'p-value: narrative 1.0000e-3 vs table 2.0000e-1',
          }
        : {
            status,
            reason: 'table has no configured p-value column/value for claimed p-value',
          }
  return {
    n_checked: 1,
    n_verified: status === 'verified' ? 1 : 0,
    n_mismatch: status === 'mismatch' ? 1 : 0,
    n_unverifiable: status === 'unverifiable' ? 1 : 0,
    verdicts: [{ claim, status: verdictStatus, strength: 'prespecified' }],
  }
}

describe('ClaimsTab', () => {
  beforeEach(() => {
    vi.resetAllMocks()
  })

  it('renders mismatch results in the Claims tab and expands verdict detail', async () => {
    mockGetTaskResult.mockResolvedValue({
      task_id: 'analyze_1',
      status: 'completed',
      description: 'Analyze differential expression',
      kind: {},
      artifacts: [],
      verification: report('mismatch'),
    })

    const user = userEvent.setup()
    render(<ClaimsTab sessionId="session-1" dag={completedDag('analyze_1')} />)

    expect(await screen.findByTestId('claims-summary')).toHaveTextContent(
      'MISMATCH 1',
    )
    expect(screen.getByTestId('claims-row-analyze_1')).toHaveTextContent(
      'MISMATCH',
    )

    await user.click(screen.getByTestId('claims-row-analyze_1'))

    expect(screen.getByText('ACAN')).toBeInTheDocument()
    expect(screen.getByText('mismatch')).toBeInTheDocument()
    expect(
      screen.getByText(/narrative 1\.0000e-3 vs table 2\.0000e-1/),
    ).toBeInTheDocument()
  })

  it('classifies all-unverifiable as UNVERIFIED rather than green PASS', async () => {
    mockGetTaskResult.mockResolvedValue({
      task_id: 'analyze_1',
      status: 'completed',
      description: 'Analyze differential expression',
      kind: {},
      artifacts: [],
      verification: report('unverifiable'),
    })

    render(<ClaimsTab sessionId="session-1" dag={completedDag('analyze_1')} />)

    expect(await screen.findByTestId('claims-summary')).toHaveTextContent(
      'UNVERIFIED 1',
    )
    expect(screen.getByTestId('claims-row-analyze_1')).toHaveTextContent(
      'UNVERIFIED',
    )
    expect(screen.getByTestId('claims-row-analyze_1')).not.toHaveTextContent(
      /\bPASS\b/,
    )
  })

  it('renders a Prespecified strength badge for prespecified claims', async () => {
    const r = report('verified')
    // The fixture builder defaults to prespecified — assert it surfaces.
    mockGetTaskResult.mockResolvedValue({
      task_id: 'analyze_1',
      status: 'completed',
      description: 'desc',
      kind: {},
      artifacts: [],
      verification: r,
    })
    const user = userEvent.setup()
    render(<ClaimsTab sessionId="session-1" dag={completedDag('analyze_1')} />)
    expect(await screen.findByTestId('claims-summary')).toHaveTextContent('PASS 1')
    await user.click(screen.getByTestId('claims-row-analyze_1'))
    expect(screen.getByTestId('strength-badge-prespecified')).toBeInTheDocument()
    expect(screen.queryByTestId('strength-badge-post_hoc')).not.toBeInTheDocument()
  })

  it('renders a Post-hoc strength badge when a verdict is demoted', async () => {
    const base = report('verified')
    base.verdicts[0]!.strength = 'post_hoc'
    mockGetTaskResult.mockResolvedValue({
      task_id: 'analyze_1',
      status: 'completed',
      description: 'desc',
      kind: {},
      artifacts: [],
      verification: base,
    })
    const user = userEvent.setup()
    render(<ClaimsTab sessionId="session-1" dag={completedDag('analyze_1')} />)
    expect(await screen.findByTestId('claims-summary')).toHaveTextContent('PASS 1')
    await user.click(screen.getByTestId('claims-row-analyze_1'))
    const badge = screen.getByTestId('strength-badge-post_hoc')
    expect(badge).toBeInTheDocument()
    expect(badge).toHaveTextContent(/post-hoc/i)
  })

  it('suppresses the strength badge for exploratory verdicts', async () => {
    const base = report('verified')
    base.verdicts[0]!.strength = 'exploratory'
    mockGetTaskResult.mockResolvedValue({
      task_id: 'analyze_1',
      status: 'completed',
      description: 'desc',
      kind: {},
      artifacts: [],
      verification: base,
    })
    const user = userEvent.setup()
    render(<ClaimsTab sessionId="session-1" dag={completedDag('analyze_1')} />)
    expect(await screen.findByTestId('claims-summary')).toHaveTextContent('PASS 1')
    await user.click(screen.getByTestId('claims-row-analyze_1'))
    expect(screen.queryByTestId('strength-badge-prespecified')).not.toBeInTheDocument()
    expect(screen.queryByTestId('strength-badge-post_hoc')).not.toBeInTheDocument()
    expect(screen.queryByTestId('strength-badge-exploratory')).not.toBeInTheDocument()
  })

  it('renders a clickable runtime decision log link when the report includes the path', async () => {
    const base = report('verified')
    base.runtime_decision_log_path = 'runtime/outputs/analyze_1/runtime-decisions.jsonl'
    mockGetTaskResult.mockResolvedValue({
      task_id: 'analyze_1',
      status: 'completed',
      description: 'desc',
      kind: {},
      artifacts: [],
      verification: base,
    })
    const user = userEvent.setup()
    render(<ClaimsTab sessionId="session-1" dag={completedDag('analyze_1')} />)
    expect(await screen.findByTestId('claims-summary')).toHaveTextContent('PASS 1')
    await user.click(screen.getByTestId('claims-row-analyze_1'))
    const link = screen.getByTestId('runtime-decision-log-link')
    expect(link).toBeInTheDocument()
    expect(link).toHaveAttribute(
      'href',
      '/artifacts/runtime/outputs/analyze_1/runtime-decisions.jsonl',
    )
  })

  it('omits the runtime decision log link when the path is absent', async () => {
    mockGetTaskResult.mockResolvedValue({
      task_id: 'analyze_1',
      status: 'completed',
      description: 'desc',
      kind: {},
      artifacts: [],
      verification: report('verified'),
    })
    const user = userEvent.setup()
    render(<ClaimsTab sessionId="session-1" dag={completedDag('analyze_1')} />)
    expect(await screen.findByTestId('claims-summary')).toHaveTextContent('PASS 1')
    await user.click(screen.getByTestId('claims-row-analyze_1'))
    expect(screen.queryByTestId('runtime-decision-log-link')).not.toBeInTheDocument()
  })

  it('re-verifies a task and refreshes the visible status', async () => {
    mockGetTaskResult.mockResolvedValue({
      task_id: 'analyze_1',
      status: 'completed',
      description: 'Analyze differential expression',
      kind: {},
      artifacts: [],
      verification: report('mismatch'),
    })
    mockVerifyTask.mockResolvedValue({ report: report('verified') })

    const user = userEvent.setup()
    render(<ClaimsTab sessionId="session-1" dag={completedDag('analyze_1')} />)

    expect(await screen.findByTestId('claims-summary')).toHaveTextContent(
      'MISMATCH 1',
    )

    await user.click(screen.getByRole('button', { name: 'Re-verify' }))

    await waitFor(() => {
      expect(screen.getByTestId('claims-summary')).toHaveTextContent('PASS 1')
    })
    expect(mockVerifyTask).toHaveBeenCalledWith('session-1', 'analyze_1')
  })
})
