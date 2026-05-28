import { describe, expect, it } from 'vitest'
import { render, screen } from '@testing-library/react'
import ToolCallStatusPill from './ToolCallStatusPill'

describe('ToolCallStatusPill', () => {
  it('renders the supplied status line', () => {
    render(<ToolCallStatusPill statusLine="Checking the plan against your description…" />)
    expect(
      screen.getByText('Checking the plan against your description…'),
    ).toBeInTheDocument()
  })

  it('exposes itself as a polite live region for assistive tech', () => {
    render(<ToolCallStatusPill statusLine="Updating the plan…" />)
    const status = screen.getByRole('status')
    expect(status).toHaveAttribute('aria-live', 'polite')
    expect(status).toHaveTextContent('Updating the plan…')
  })

  it('keeps the spinner decorative so it is hidden from screen readers', () => {
    const { container } = render(
      <ToolCallStatusPill statusLine="Looking up analysis details…" />,
    )
    const decorative = container.querySelector('[aria-hidden="true"]')
    expect(decorative).not.toBeNull()
  })
})
