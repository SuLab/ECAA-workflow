import { describe, expect, it, vi } from 'vitest'
import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import NeedsInputChip from './NeedsInputChip'

describe('NeedsInputChip', () => {
  it('renders nothing when no tasks are blocked', () => {
    const { container } = render(
      <NeedsInputChip blockedTasks={[]} onJump={vi.fn()} />,
    )
    expect(container.firstChild).toBeNull()
  })

  it('renders a singular chip label when exactly one task is blocked', () => {
    render(
      <NeedsInputChip
        blockedTasks={['biological_interpretation']}
        onJump={vi.fn()}
      />,
    )
    const chip = screen.getByTestId('needs-input-chip')
    expect(chip).toHaveTextContent(/Needs your input \(1 step\)/)
  })

  it('renders a plural chip label when multiple tasks are blocked', () => {
    render(
      <NeedsInputChip
        blockedTasks={['bio_interp', 'cell_communication', 'trajectory']}
        onJump={vi.fn()}
      />,
    )
    expect(screen.getByTestId('needs-input-chip')).toHaveTextContent(
      /Needs your input \(3 steps\)/,
    )
  })

  it('fires onJump with the first blocked task id when clicked', async () => {
    const onJump = vi.fn()
    render(
      <NeedsInputChip
        blockedTasks={['first_blocked', 'second_blocked']}
        onJump={onJump}
      />,
    )
    const user = userEvent.setup()
    await user.click(screen.getByTestId('needs-input-chip'))
    expect(onJump).toHaveBeenCalledWith('first_blocked')
  })

  it('carries an aria-live attribute so screen readers announce new blockers', () => {
    render(
      <NeedsInputChip blockedTasks={['x']} onJump={vi.fn()} />,
    )
    expect(screen.getByTestId('needs-input-chip')).toHaveAttribute(
      'aria-live',
      'polite',
    )
  })
})
