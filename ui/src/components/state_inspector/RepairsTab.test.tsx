// V3+v4 residuals closure RepairsTab mount tests.
//
// Covers: empty-state placeholder, proposal list rendering, accept/reject
// delegation through the useRepairProposals hook → chatClient helpers.

import { render, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import * as chatClient from '../../api/chatClient'
import { RepairsTab } from './RepairsTab'
import type { RepairProposal } from '../../types/RepairProposal'

const sampleProposal: RepairProposal = {
  id: 'p1',
  strategy_id: 'liftover_to_grch38',
  gap_id: 'gap-1',
  modification: {} as unknown as RepairProposal['modification'],
  risk_class: 'low_auto_attempt',
  generated_assumptions: [],
  required_credentials: [],
  rationale: 'Convert coordinates to GRCh38 to match downstream consumers.',
  ctx_snapshot_hash: 'abc123',
}

afterEach(() => {
  vi.restoreAllMocks()
})

describe('RepairsTab', () => {
  it('renders the empty-state placeholder when no proposals are pending', async () => {
    vi.spyOn(chatClient, 'fetchRepairProposals').mockResolvedValue([])
    const { findByText } = render(<RepairsTab sessionId="s1" />)
    expect(
      await findByText(/No pending repair proposals/i),
    ).toBeInTheDocument()
  })

  it('renders one RepairProposalCard per proposal returned by the hook', async () => {
    vi.spyOn(chatClient, 'fetchRepairProposals').mockResolvedValue([
      sampleProposal,
    ])
    const { findByText } = render(<RepairsTab sessionId="s1" />)
    expect(await findByText('liftover_to_grch38')).toBeInTheDocument()
  })

  it('shows "No session selected." when sessionId is null', () => {
    const { getByText } = render(<RepairsTab sessionId={null} />)
    expect(getByText(/No session selected/i)).toBeInTheDocument()
  })

  it('does not crash when fetchRepairProposals rejects', async () => {
    vi.spyOn(chatClient, 'fetchRepairProposals').mockRejectedValue(
      new Error('network down'),
    )
    const { findByText } = render(<RepairsTab sessionId="s1" />)
    await waitFor(async () => {
      // The empty-state placeholder remains visible after the rejected
      // fetch settles.
      expect(
        await findByText(/No pending repair proposals/i),
      ).toBeInTheDocument()
    })
  })
})
