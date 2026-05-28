import { describe, expect, it, vi } from 'vitest'
import { render, screen, fireEvent } from '@testing-library/react'
import PackageSafetyBanner from './PackageSafetyBanner'
import type { SafetySummary } from '../types/SafetySummary'

function summary(overrides: Partial<SafetySummary> = {}): SafetySummary {
  return {
    worst_case_level: 'compute',
    level_counts: { safe: 2, network: 1, compute: 5, exec: 0 },
    ...overrides,
  }
}

describe('PackageSafetyBanner — Phase 8.3 (atom-safety-policy plan)', () => {
  it('renders all four per-level count pills', () => {
    render(
      <PackageSafetyBanner summary={summary()} onFilterByLevel={vi.fn()} />,
    )
    expect(screen.getByRole('button', { name: /2 safe/i })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /1 network/i })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /5 compute/i })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /0 exec/i })).toBeInTheDocument()
  })

  it('shows the worst-case level', () => {
    render(
      <PackageSafetyBanner
        summary={summary({ worst_case_level: 'exec' })}
        onFilterByLevel={vi.fn()}
      />,
    )
    expect(screen.getByText(/worst-case/i)).toBeInTheDocument()
    expect(screen.getByText(/exec/i, { selector: 'strong' })).toBeInTheDocument()
  })

  it('invokes onFilterByLevel with the matching level when a pill is clicked', () => {
    const onFilter = vi.fn()
    render(<PackageSafetyBanner summary={summary()} onFilterByLevel={onFilter} />)
    fireEvent.click(screen.getByRole('button', { name: /5 compute/i }))
    expect(onFilter).toHaveBeenCalledWith('compute')
    fireEvent.click(screen.getByRole('button', { name: /1 network/i }))
    expect(onFilter).toHaveBeenCalledWith('network')
  })
})
