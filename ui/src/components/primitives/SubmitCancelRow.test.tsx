// Primitive coverage. SubmitCancelRow renders the
// primary + secondary action pair under every interactive turn card,
// so its busy/disabled/submitDisabled semantics are load-bearing.

import { fireEvent, render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import { SubmitCancelRow } from './SubmitCancelRow'

describe('SubmitCancelRow', () => {
  it('fires onSubmit when the primary button is clicked', () => {
    const onSubmit = vi.fn()
    render(<SubmitCancelRow onSubmit={onSubmit} />)
    fireEvent.click(screen.getByRole('button', { name: 'Submit' }))
    expect(onSubmit).toHaveBeenCalledTimes(1)
  })

  it('renders the cancel button only when onCancel is provided', () => {
    const { rerender } = render(<SubmitCancelRow onSubmit={vi.fn()} />)
    expect(screen.queryByRole('button', { name: 'Cancel' })).toBeNull()
    rerender(<SubmitCancelRow onSubmit={vi.fn()} onCancel={vi.fn()} />)
    expect(screen.getByRole('button', { name: 'Cancel' })).toBeInTheDocument()
  })

  it('shows "Working…" and disables both buttons when busy', () => {
    render(
      <SubmitCancelRow onSubmit={vi.fn()} onCancel={vi.fn()} busy />
    )
    expect(screen.getByRole('button', { name: 'Working…' })).toBeDisabled()
    expect(screen.getByRole('button', { name: 'Cancel' })).toBeDisabled()
    expect(screen.queryByRole('button', { name: 'Submit' })).toBeNull()
  })

  it('disables only the submit when submitDisabled is set', () => {
    render(
      <SubmitCancelRow
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
        submitDisabled
      />
    )
    expect(screen.getByRole('button', { name: 'Submit' })).toBeDisabled()
    expect(screen.getByRole('button', { name: 'Cancel' })).not.toBeDisabled()
  })

  it('disables both buttons in the broader `disabled` mode without swapping the label', () => {
    render(<SubmitCancelRow onSubmit={vi.fn()} onCancel={vi.fn()} disabled />)
    const submit = screen.getByRole('button', { name: 'Submit' })
    expect(submit).toBeDisabled()
    expect(screen.getByRole('button', { name: 'Cancel' })).toBeDisabled()
  })

  it('honours custom labels', () => {
    render(
      <SubmitCancelRow
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
        submitLabel="Apply amendment"
        cancelLabel="Discard"
      />
    )
    expect(screen.getByRole('button', { name: 'Apply amendment' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Discard' })).toBeInTheDocument()
  })
})
