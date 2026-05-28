// Primitive coverage. OptionalTextarea is the
// rationale/notes input shared by 5 turn cards (branch, rerun,
// amend, sensitivity, unblock) — verifying the onChange + ariaLabel
// + disabled contract guards the SME's primary text-input surface.

import { fireEvent, render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import { OptionalTextarea } from './OptionalTextarea'

describe('OptionalTextarea', () => {
  it('renders the label and the textarea', () => {
    render(
      <OptionalTextarea
        label="Optional rationale"
        value=""
        onChange={vi.fn()}
      />
    )
    expect(screen.getByText('Optional rationale')).toBeInTheDocument()
    expect(screen.getByRole('textbox', { name: 'Optional rationale' })).toBeInTheDocument()
  })

  it('uses the explicit ariaLabel over the visible label', () => {
    render(
      <OptionalTextarea
        label="Notes"
        ariaLabel="Why are you branching?"
        value=""
        onChange={vi.fn()}
      />
    )
    expect(
      screen.getByRole('textbox', { name: 'Why are you branching?' })
    ).toBeInTheDocument()
  })

  it('fires onChange with the new value as the user types', () => {
    const onChange = vi.fn()
    render(
      <OptionalTextarea
        label="Rationale"
        value=""
        onChange={onChange}
      />
    )
    const ta = screen.getByRole('textbox', { name: 'Rationale' })
    fireEvent.change(ta, { target: { value: 'better cell-type prior' } })
    expect(onChange).toHaveBeenCalledWith('better cell-type prior')
  })

  it('honours the disabled prop', () => {
    render(
      <OptionalTextarea
        label="Rationale"
        value="x"
        onChange={vi.fn()}
        disabled
      />
    )
    expect(screen.getByRole('textbox', { name: 'Rationale' })).toBeDisabled()
  })

  it('uses the default rows count when none is provided', () => {
    render(<OptionalTextarea label="Rationale" value="" onChange={vi.fn()} />)
    const ta = screen.getByRole('textbox', { name: 'Rationale' })
    expect(ta).toHaveAttribute('rows', '3')
  })

  it('honours an explicit rows count', () => {
    render(
      <OptionalTextarea
        label="Rationale"
        rows={6}
        value=""
        onChange={vi.fn()}
      />
    )
    expect(
      screen.getByRole('textbox', { name: 'Rationale' })
    ).toHaveAttribute('rows', '6')
  })
})
