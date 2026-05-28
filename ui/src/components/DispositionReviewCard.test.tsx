import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import DispositionReviewCard from './DispositionReviewCard'
import type {
  DispositionBodyWire,
  DispositionListEntryWire,
} from '../api/chatClient'

// Mock the REST client so the card never hits the network.
vi.mock('../api/chatClient', () => {
  return {
    applyDisposition: vi.fn(),
    rejectDisposition: vi.fn(),
    getDisposition: vi.fn(),
  }
})

const pendingEntry: DispositionListEntryWire = {
  path: 'runtime/outputs/results_review/sme_disposition.json',
  task_id: 'results_review',
  status: 'pending',
  schema_version: 1,
  action_count: 2,
  created_at: '2026-04-24T00:09:56Z',
  authoritative_interpretation: 'PC1 residual batch signal dominates.',
}

const body: DispositionBodyWire = {
  schema_version: 1,
  task_id: 'results_review',
  created_at: '2026-04-24T00:09:56Z',
  authoritative_interpretation: 'PC1 residual batch signal dominates.',
  actions: [
    {
      kind: 'amend_method',
      target_stage: 'batch_correction',
      new_method: 'cca_integratelayers',
      rationale: 'switch from harmony',
    },
    {
      kind: 'invalidate_slice',
      from_stage: 'batch_correction',
      stages_explicit: [
        'integration',
        'clustering',
        'cell_type_annotation',
      ],
    },
  ],
  auto_apply: false,
  status: 'pending',
}

describe('DispositionReviewCard', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })
  afterEach(() => {
    vi.clearAllMocks()
  })

  it('renders the action list with human-readable summaries (pending)', () => {
    render(
      <DispositionReviewCard
        sessionId="s1"
        entry={pendingEntry}
        body={body}
      />,
    )
    expect(
      screen.getByText(/The agent proposes 2 changes/i),
    ).toBeInTheDocument()
    expect(
      screen.getByText(/Amend .* → cca_integratelayers/i),
    ).toBeInTheDocument()
    expect(
      screen.getByText(/Invalidate 3 downstream stages/i),
    ).toBeInTheDocument()
  })

  it('Apply all calls applyDisposition with the rationale', async () => {
    const { applyDisposition } = await import('../api/chatClient')
    ;(applyDisposition as ReturnType<typeof vi.fn>).mockResolvedValue({
      applied: 2,
      failed: 0,
      invalidated_tasks: ['integration', 'clustering'],
      status: 'applied',
      errors: [],
    })
    const onDone = vi.fn().mockResolvedValue(undefined)
    const user = userEvent.setup()
    render(
      <DispositionReviewCard
        sessionId="s1"
        entry={pendingEntry}
        body={body}
        onDone={onDone}
      />,
    )
    await user.type(
      screen.getByTestId('disposition-rationale'),
      'switching to CCA',
    )
    await user.click(screen.getByTestId('disposition-apply-all'))
    await waitFor(() => {
      expect(applyDisposition).toHaveBeenCalledWith('s1', pendingEntry.path, {
        rationale: 'switching to CCA',
      })
    })
    expect(onDone).toHaveBeenCalled()
  })

  it('Reject calls rejectDisposition with the rationale', async () => {
    const { rejectDisposition } = await import('../api/chatClient')
    ;(rejectDisposition as ReturnType<typeof vi.fn>).mockResolvedValue({
      status: 'rejected',
    })
    const onDone = vi.fn().mockResolvedValue(undefined)
    const user = userEvent.setup()
    render(
      <DispositionReviewCard
        sessionId="s1"
        entry={pendingEntry}
        body={body}
        onDone={onDone}
      />,
    )
    await user.type(
      screen.getByTestId('disposition-rationale'),
      'not ready',
    )
    await user.click(screen.getByTestId('disposition-reject'))
    await waitFor(() => {
      expect(rejectDisposition).toHaveBeenCalledWith(
        's1',
        pendingEntry.path,
        'not ready',
      )
    })
    expect(onDone).toHaveBeenCalled()
  })

  it('applied entry renders a collapsed status row without buttons', () => {
    render(
      <DispositionReviewCard
        sessionId="s1"
        entry={{ ...pendingEntry, status: 'applied' }}
        body={{ ...body, status: 'applied', status_updated_at: '2026-04-24T00:10:00Z' }}
      />,
    )
    expect(screen.getByText(/Applied/)).toBeInTheDocument()
    expect(screen.queryByTestId('disposition-apply-all')).not.toBeInTheDocument()
    expect(screen.queryByTestId('disposition-reject')).not.toBeInTheDocument()
  })

  it('rejected entry renders a collapsed status row without buttons', () => {
    render(
      <DispositionReviewCard
        sessionId="s1"
        entry={{ ...pendingEntry, status: 'rejected' }}
        body={{ ...body, status: 'rejected', status_updated_at: '2026-04-24T00:10:00Z' }}
      />,
    )
    expect(screen.getByText(/Rejected/)).toBeInTheDocument()
    expect(screen.queryByTestId('disposition-apply-all')).not.toBeInTheDocument()
  })

  it('partial status keeps the card interactive with Retry copy', () => {
    render(
      <DispositionReviewCard
        sessionId="s1"
        entry={{ ...pendingEntry, status: 'partial' }}
        body={{ ...body, status: 'partial' }}
      />,
    )
    const button = screen.getByTestId('disposition-apply-all')
    expect(button).toBeEnabled()
    expect(button).toHaveTextContent(/Retry/)
  })

  it('renders an empty-action message when actions[] is empty', () => {
    render(
      <DispositionReviewCard
        sessionId="s1"
        entry={{ ...pendingEntry, action_count: 0 }}
        body={{ ...body, actions: [] }}
      />,
    )
    expect(
      screen.getByText(/no applicable actions/i),
    ).toBeInTheDocument()
    // The Apply button is disabled when there are no actions.
    const btn = screen.getByTestId('disposition-apply-all')
    expect(btn).toBeDisabled()
  })

  it('shows per-action error rows when apply returns partial', async () => {
    const { applyDisposition } = await import('../api/chatClient')
    ;(applyDisposition as ReturnType<typeof vi.fn>).mockResolvedValue({
      applied: 1,
      failed: 1,
      invalidated_tasks: [],
      status: 'partial',
      errors: [
        {
          action_index: 1,
          action_kind: 'amend_method',
          target_stage: 'annotation',
          reason: 'session is not in Emitted state',
        },
      ],
    })
    const user = userEvent.setup()
    render(
      <DispositionReviewCard
        sessionId="s1"
        entry={pendingEntry}
        body={body}
      />,
    )
    await user.click(screen.getByTestId('disposition-apply-all'))
    await waitFor(() => {
      expect(
        screen.getByText(/session is not in Emitted state/i),
      ).toBeInTheDocument()
    })
  })
})
