import { describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen } from '@testing-library/react'
import StillThinkingIndicator from './StillThinkingIndicator'

describe('StillThinkingIndicator', () => {
  it('renders a polite live region with the still-thinking copy', () => {
    render(<StillThinkingIndicator />)
    const status = screen.getByRole('status')
    expect(status).toHaveAttribute('aria-live', 'polite')
    expect(status).toHaveTextContent(/still thinking/i)
  })

  it('keeps the pulsing dot decorative', () => {
    const { container } = render(<StillThinkingIndicator />)
    const decorative = container.querySelector('[aria-hidden="true"]')
    expect(decorative).not.toBeNull()
  })

  it('does not call out specific tools — distinct from the per-tool pill', () => {
    // The 8s whole-turn indicator is intentionally tool-agnostic so it
    // stays on while a multi-tool sequence runs.
    render(<StillThinkingIndicator />)
    expect(screen.getByRole('status').textContent).not.toMatch(/tool/i)
  })

  // D8 mitigation — progressive escalation through the four stages.
  // The Anthropic side can stall for tens of seconds; the chip's
  // progressive messages tell the SME how concerned to be without
  // anyone having to refresh.
  it('escalates copy at the slow (30s) stage', () => {
    render(<StillThinkingIndicator stage="slow" />)
    expect(screen.getByRole('status').textContent ?? '').toMatch(
      /anthropic is slow/i,
    )
  })

  it('escalates copy at the very_slow (60s) stage', () => {
    render(<StillThinkingIndicator stage="very_slow" />)
    expect(screen.getByRole('status').textContent ?? '').toMatch(
      /longer than usual/i,
    )
  })

  it('shows a Cancel button at the cancelable (90s) stage', () => {
    const onCancel = vi.fn()
    render(<StillThinkingIndicator stage="cancelable" onCancel={onCancel} />)
    const btn = screen.getByRole('button', { name: /cancel/i })
    expect(btn).not.toBeNull()
    fireEvent.click(btn)
    expect(onCancel).toHaveBeenCalledTimes(1)
  })

  it('does not show a Cancel button before the cancelable stage', () => {
    render(<StillThinkingIndicator stage="slow" onCancel={vi.fn()} />)
    expect(screen.queryByRole('button', { name: /cancel/i })).toBeNull()
  })

  it('does not show a Cancel button at cancelable when onCancel is unset', () => {
    render(<StillThinkingIndicator stage="cancelable" />)
    expect(screen.queryByRole('button', { name: /cancel/i })).toBeNull()
  })
})
