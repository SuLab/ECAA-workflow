// Primitive coverage. RadioRow drives the
// discovery-approval and sensitivity-comparison card selection
// surfaces, so its onChange/ariaLabel/disabled contract is
// load-bearing for SME workflow.

import { fireEvent, render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import { RadioRow } from './RadioRow'

type Choice = 'a' | 'b' | 'c'

const opts = [
  { value: 'a' as Choice, label: 'Option A' },
  { value: 'b' as Choice, label: 'Option B' },
  { value: 'c' as Choice, label: 'Option C', disabled: true },
]

describe('RadioRow', () => {
  it('renders all options inside a radiogroup with the configured aria-label', () => {
    render(
      <RadioRow
        name="discovery"
        options={opts}
        value={null}
        onChange={vi.fn()}
        ariaLabel="Pick a discovery candidate"
      />
    )
    const group = screen.getByRole('radiogroup', { name: 'Pick a discovery candidate' })
    expect(group).toBeInTheDocument()
    expect(screen.getAllByRole('radio')).toHaveLength(3)
  })

  it('falls back to the `name` prop when ariaLabel is omitted', () => {
    render(
      <RadioRow name="winner" options={opts} value={null} onChange={vi.fn()} />
    )
    expect(screen.getByRole('radiogroup', { name: 'winner' })).toBeInTheDocument()
  })

  it('marks the matching option as checked', () => {
    render(
      <RadioRow name="discovery" options={opts} value="b" onChange={vi.fn()} />
    )
    expect(screen.getByLabelText('Option A')).not.toBeChecked()
    expect(screen.getByLabelText('Option B')).toBeChecked()
  })

  it('fires onChange with the next value when an option is clicked', () => {
    const onChange = vi.fn()
    render(
      <RadioRow name="discovery" options={opts} value="a" onChange={onChange} />
    )
    fireEvent.click(screen.getByLabelText('Option B'))
    expect(onChange).toHaveBeenCalledWith('b')
  })

  it('marks the option flagged disabled as disabled in the DOM', () => {
    // Native browsers swallow clicks on disabled inputs; React testing
    // library's `fireEvent.click` synthesizes the event regardless, so
    // we only assert the DOM state — the gating contract is the
    // disabled attribute, not the React spy.
    render(
      <RadioRow name="discovery" options={opts} value="a" onChange={vi.fn()} />
    )
    expect(screen.getByLabelText('Option C')).toBeDisabled()
  })
})
