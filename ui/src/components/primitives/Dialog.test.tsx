// Dialog primitive coverage. The Dialog wraps 8
// across-codebase modal components (Share, TaskDetail, TaskLog,
// EdgeProof, ExplainButton, ClinicalConfirmGate, the MetricsTab
// modal subview, CommandPalette), so silent regressions here
// propagate everywhere.

import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import axe from 'axe-core'
import { Dialog } from './Dialog'

describe('Dialog', () => {
  it('renders children inside a role="dialog" aria-modal=true container', () => {
    render(
      <Dialog onClose={vi.fn()} ariaLabel="Test dialog">
        <button type="button">First action</button>
        <button type="button">Second action</button>
      </Dialog>,
    )
    const dialog = screen.getByRole('dialog', { name: 'Test dialog' })
    expect(dialog).toBeInTheDocument()
    expect(dialog).toHaveAttribute('aria-modal', 'true')
    expect(screen.getByRole('button', { name: 'First action' })).toBeInTheDocument()
  })

  it('calls onClose when Escape is pressed', () => {
    const onClose = vi.fn()
    render(
      <Dialog onClose={onClose} ariaLabel="Esc test">
        <button type="button">Inside</button>
      </Dialog>,
    )
    fireEvent.keyDown(window, { key: 'Escape' })
    expect(onClose).toHaveBeenCalledTimes(1)
  })

  it('calls onClose on backdrop mousedown when closeOnOutsideClick is on', () => {
    const onClose = vi.fn()
    render(
      <Dialog onClose={onClose} ariaLabel="Outside test">
        <button type="button">Inside</button>
      </Dialog>,
    )
    const backdrop = screen.getByRole('dialog', { name: 'Outside test' })
    fireEvent.mouseDown(backdrop, { target: backdrop, currentTarget: backdrop })
    expect(onClose).toHaveBeenCalledTimes(1)
  })

  it('does NOT call onClose on body clicks (events stop at the content wrapper)', () => {
    const onClose = vi.fn()
    render(
      <Dialog onClose={onClose} ariaLabel="Body test">
        <button type="button" data-testid="inside">Inside</button>
      </Dialog>,
    )
    fireEvent.mouseDown(screen.getByTestId('inside'))
    expect(onClose).not.toHaveBeenCalled()
  })

  it('respects closeOnOutsideClick={false}', () => {
    const onClose = vi.fn()
    render(
      <Dialog onClose={onClose} ariaLabel="Locked test" closeOnOutsideClick={false}>
        <button type="button">Inside</button>
      </Dialog>,
    )
    const backdrop = screen.getByRole('dialog', { name: 'Locked test' })
    fireEvent.mouseDown(backdrop, { target: backdrop, currentTarget: backdrop })
    expect(onClose).not.toHaveBeenCalled()
  })

  it('focuses the first interactive element on mount', async () => {
    render(
      <Dialog onClose={vi.fn()} ariaLabel="Focus test">
        <button type="button">First</button>
        <button type="button">Second</button>
      </Dialog>,
    )
    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'First' })).toHaveFocus()
    })
  })

  it('traps Tab focus inside the dialog (last → first)', async () => {
    render(
      <Dialog onClose={vi.fn()} ariaLabel="Trap test">
        <button type="button">First</button>
        <button type="button">Last</button>
      </Dialog>,
    )
    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'First' })).toHaveFocus()
    })
    // Move to last manually, then Tab — should wrap to first.
    const last = screen.getByRole('button', { name: 'Last' })
    last.focus()
    expect(last).toHaveFocus()
    fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Tab' })
    expect(screen.getByRole('button', { name: 'First' })).toHaveFocus()
  })

  it('traps Shift+Tab focus (first → last)', async () => {
    render(
      <Dialog onClose={vi.fn()} ariaLabel="ShiftTab test">
        <button type="button">First</button>
        <button type="button">Last</button>
      </Dialog>,
    )
    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'First' })).toHaveFocus()
    })
    fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Tab', shiftKey: true })
    expect(screen.getByRole('button', { name: 'Last' })).toHaveFocus()
  })

  it('passes axe-core WCAG AA audit', async () => {
    const { container } = render(
      <Dialog onClose={vi.fn()} ariaLabel="Axe-audited dialog">
        <h2 id="dlg-heading">Heading</h2>
        <p>Body content with a description.</p>
        <button type="button">Confirm</button>
        <button type="button">Cancel</button>
      </Dialog>,
    )
    const results = await axe.run(container, {
      runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa'] },
      rules: { 'color-contrast': { enabled: false } },
    })
    expect(results.violations).toEqual([])
  })
})
