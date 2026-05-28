import { describe, expect, it, vi } from 'vitest'
import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import ConfirmationTurnCard from './ConfirmationTurnCard'
import { SessionTestWrapper } from '../test/sessionTestHelpers'

const card = {
  summary_markdown:
    "Here's the plan: 47 IVD scRNA-seq libraries, degenerated vs healthy.\n\n" +
    'Reports statistical patterns, not causal claims.',
  summary_hash: 'a'.repeat(64),
}

describe('ConfirmationTurnCard', () => {
  it('renders the summary text and both action buttons', () => {
    render(
      <SessionTestWrapper>
        <ConfirmationTurnCard
          card={card}
          onConfirm={vi.fn()}
          onReject={vi.fn()}
        />
      </SessionTestWrapper>,
    )
    expect(screen.getByText(/47 IVD scRNA-seq libraries/i)).toBeInTheDocument()
    expect(
      screen.getByRole('button', { name: /^accept$/i }),
    ).toBeInTheDocument()
    expect(
      screen.getByRole('button', { name: /revise/i }),
    ).toBeInTheDocument()
  })

  it('fires onConfirm when the SME clicks Accept', async () => {
    const user = userEvent.setup()
    const onConfirm = vi.fn().mockResolvedValue(undefined)
    render(
      <SessionTestWrapper>
        <ConfirmationTurnCard
          card={card}
          onConfirm={onConfirm}
          onReject={vi.fn()}
        />
      </SessionTestWrapper>,
    )
    await user.click(screen.getByRole('button', { name: /^accept$/i }))
    expect(onConfirm).toHaveBeenCalledOnce()
  })

  it('shows the post-decision indicator after Accept', async () => {
    const user = userEvent.setup()
    render(
      <SessionTestWrapper>
        <ConfirmationTurnCard
          card={card}
          onConfirm={vi.fn().mockResolvedValue(undefined)}
          onReject={vi.fn()}
        />
      </SessionTestWrapper>,
    )
    await user.click(screen.getByRole('button', { name: /^accept$/i }))
    expect(
      await screen.findByText(/accepted — continuing/i),
    ).toBeInTheDocument()
    // Buttons should be gone now
    expect(screen.queryByRole('button', { name: /^accept$/i })).toBeNull()
  })

  it('fires onReject when the SME clicks Revise', async () => {
    const user = userEvent.setup()
    const onReject = vi.fn().mockResolvedValue(undefined)
    render(
      <SessionTestWrapper>
        <ConfirmationTurnCard
          card={card}
          onConfirm={vi.fn()}
          onReject={onReject}
        />
      </SessionTestWrapper>,
    )
    await user.click(screen.getByRole('button', { name: /revise/i }))
    expect(onReject).toHaveBeenCalledOnce()
  })

  it('resets to Pending after Revise resolves so the card never gets stuck (S5.11)', async () => {
    const user = userEvent.setup()
    const onReject = vi.fn().mockResolvedValue(undefined)
    render(
      <SessionTestWrapper>
        <ConfirmationTurnCard
          card={card}
          onConfirm={vi.fn()}
          onReject={onReject}
        />
      </SessionTestWrapper>,
    )
    await user.click(screen.getByRole('button', { name: /revise/i }))
    // After onReject resolves the card is back in pending state — both
    // buttons render again and neither is locked.
    expect(
      await screen.findByRole('button', { name: /^accept$/i }),
    ).toBeInTheDocument()
    expect(
      screen.getByRole('button', { name: /revise/i }),
    ).toBeInTheDocument()
    // The transient "returning" indicator is gone.
    expect(screen.queryByText(/returning to the conversation/i)).toBeNull()
  })

  it('disables both buttons when disabled prop is set', () => {
    render(
      <SessionTestWrapper>
        <ConfirmationTurnCard
          card={card}
          onConfirm={vi.fn()}
          onReject={vi.fn()}
          disabled
        />
      </SessionTestWrapper>,
    )
    expect(screen.getByRole('button', { name: /^accept$/i })).toBeDisabled()
    expect(
      screen.getByRole('button', { name: /revise/i }),
    ).toBeDisabled()
  })
})
