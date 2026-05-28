import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render } from '@testing-library/react'
import DagFilterChips, { loadFilter } from './DagFilterChips'
import { useState } from 'react'

beforeEach(() => {
  window.localStorage.clear()
})
afterEach(() => {
  window.localStorage.clear()
})

function Wrapper() {
  const [f, setF] = useState<Set<string>>(() => loadFilter())
  return <DagFilterChips selected={f} onChange={setF} />
}

describe('DagFilterChips', () => {
  it('renders All + each status chip', () => {
    const { getByText } = render(<Wrapper />)
    expect(getByText('All')).toBeTruthy()
    expect(getByText('Blocked')).toBeTruthy()
    expect(getByText('Running')).toBeTruthy()
  })

  it('toggles a chip on click and persists to localStorage', () => {
    const onChange = vi.fn()
    const { getByText, rerender } = render(
      <DagFilterChips selected={new Set()} onChange={onChange} />,
    )
    fireEvent.click(getByText('Blocked'))
    expect(onChange).toHaveBeenCalled()
    const latest = onChange!.mock.calls[onChange.mock.calls.length - 1]![0] as Set<string>
    expect(latest.has('blocked')).toBe(true)
    // The component wrote to localStorage via saveFilter — reload via loadFilter
    rerender(<DagFilterChips selected={latest} onChange={onChange} />)
    expect(loadFilter().has('blocked')).toBe(true)
  })

  it('All clears any selection', () => {
    const onChange = vi.fn()
    const { getByText } = render(
      <DagFilterChips selected={new Set(['blocked'])} onChange={onChange} />,
    )
    fireEvent.click(getByText('All'))
    expect(onChange).toHaveBeenCalled()
    const latest = onChange!.mock.calls[onChange.mock.calls.length - 1]![0] as Set<string>
    expect(latest.size).toBe(0)
  })
})
