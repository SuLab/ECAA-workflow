// V3+v4 residuals closure RepairProposalCard accept/reject
// callback delegation + HighCredentialedReview disable behavior.

import { fireEvent, render } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { RepairProposalCard } from './RepairProposalCard'
import type { RepairProposal } from '../../types/RepairProposal'

function makeProposal(overrides: Partial<RepairProposal> = {}): RepairProposal {
  return {
    id: 'p-test',
    strategy_id: 'sample_strategy',
    gap_id: 'gap-test',
    modification: {} as unknown as RepairProposal['modification'],
    risk_class: 'low_auto_attempt',
    generated_assumptions: [],
    required_credentials: [],
    rationale: 'Test rationale.',
    ctx_snapshot_hash: 'hash',
    ...overrides,
  }
}

afterEach(() => {
  vi.restoreAllMocks()
})

describe('RepairProposalCard callback delegation', () => {
  it('delegates Accept to the onAccept callback when provided', async () => {
    const onAccept = vi.fn().mockResolvedValue(undefined)
    const { getByText } = render(
      <RepairProposalCard proposal={makeProposal()} onAccept={onAccept} />,
    )
    fireEvent.click(getByText('Accept'))
    // onAccept is called with the proposal id + an empty creds array
    // (no credentials required for low_auto_attempt).
    await vi.waitFor(() => {
      expect(onAccept).toHaveBeenCalledWith('p-test', [])
    })
  })

  it('disables Accept for HighCredentialedReview until credentials are typed', () => {
    const proposal = makeProposal({
      risk_class: 'high_credentialed_review',
      required_credentials: ['clinical_lead'],
    })
    const { getByText, getByLabelText } = render(
      <RepairProposalCard proposal={proposal} onAccept={vi.fn()} />,
    )
    const acceptButton = getByText('Accept').closest('button')!
    expect(acceptButton).toBeDisabled()
    // Typing into the credentials field flips the button on.
    fireEvent.change(getByLabelText('Your credentials'), {
      target: { value: 'clinical_lead' },
    })
    expect(acceptButton).not.toBeDisabled()
  })

  it('delegates Reject to the onReject callback when provided', async () => {
    const onReject = vi.fn().mockResolvedValue(undefined)
    const { getByText, getByLabelText } = render(
      <RepairProposalCard proposal={makeProposal()} onReject={onReject} />,
    )
    // First click reveals the reason input; second click confirms.
    fireEvent.click(getByText('Reject'))
    fireEvent.change(getByLabelText('Reason'), {
      target: { value: 'unsafe assumption' },
    })
    fireEvent.click(getByText('Confirm reject'))
    await vi.waitFor(() => {
      expect(onReject).toHaveBeenCalledWith('p-test', 'unsafe assumption')
    })
  })
})
