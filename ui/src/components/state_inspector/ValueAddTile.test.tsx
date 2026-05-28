// Coverage: ValueAddTile rendering — empty state, zero-row payload, and
// populated scorecard. The polling hook (useEffect / setInterval) is not
// exercised here; integration coverage lives in StateInspectorPane.test.tsx.

import { render } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { ValueAddTile } from './MetricsTab'
import type { ValueAddMetricsResponse } from '../../api/chatClient'

describe('ValueAddTile', () => {
  it('shows empty-state prompt when valueAdd is null', () => {
    const { getByText } = render(<ValueAddTile valueAdd={null} />)
    expect(getByText(/No value-add metrics yet/i)).toBeInTheDocument()
    expect(getByText('make value-add-eval-all')).toBeInTheDocument()
  })

  it('shows no-rows message when tier_results is empty', () => {
    const payload: ValueAddMetricsResponse = {
      tier_results: [],
      last_updated_ms: 0,
    }
    const { getByText } = render(<ValueAddTile valueAdd={payload} />)
    expect(getByText(/no tier rows yet/i)).toBeInTheDocument()
  })

  it('renders one row per tier result', () => {
    const payload: ValueAddMetricsResponse = {
      tier_results: [
        { tier: '15.1', bucket: 'A', passed: true, score: 0.82, threshold: 0.70, last_run_ms: 0 },
        { tier: '16.1', bucket: 'B', passed: false, score: 0.55, threshold: 0.75, last_run_ms: 0 },
      ],
      last_updated_ms: 0,
    }
    const { getByText, getAllByRole } = render(<ValueAddTile valueAdd={payload} />)
    // Header row + 2 data rows = 3 rows total.
    expect(getAllByRole('row')).toHaveLength(3)
    expect(getByText('15.1')).toBeInTheDocument()
    expect(getByText('16.1')).toBeInTheDocument()
  })

  it('renders PASS for passing tiers and FAIL for failing tiers', () => {
    const payload: ValueAddMetricsResponse = {
      tier_results: [
        { tier: '15.1', bucket: 'A', passed: true, score: 0.82, threshold: 0.70, last_run_ms: 0 },
        { tier: '15.2', bucket: 'A', passed: false, score: 0.30, threshold: 0.50, last_run_ms: 0 },
      ],
      last_updated_ms: 0,
    }
    const { getByText } = render(<ValueAddTile valueAdd={payload} />)
    expect(getByText('PASS')).toBeInTheDocument()
    expect(getByText('FAIL')).toBeInTheDocument()
  })

  it('formats score and threshold to 3 decimal places', () => {
    const payload: ValueAddMetricsResponse = {
      tier_results: [
        { tier: '16.2', bucket: 'B', passed: true, score: 0.9, threshold: 0.8, last_run_ms: 0 },
      ],
      last_updated_ms: 0,
    }
    const { getByText } = render(<ValueAddTile valueAdd={payload} />)
    expect(getByText('0.900')).toBeInTheDocument()
    expect(getByText('0.800')).toBeInTheDocument()
  })

  it('applies data attributes for test targeting', () => {
    const payload: ValueAddMetricsResponse = {
      tier_results: [
        { tier: '17.3', bucket: 'C', passed: false, score: 0.1, threshold: 0.5, last_run_ms: 0 },
      ],
      last_updated_ms: 0,
    }
    const { container } = render(<ValueAddTile valueAdd={payload} />)
    const row = container.querySelector('[data-tier="17.3"]')
    expect(row).not.toBeNull()
    expect(row?.getAttribute('data-bucket')).toBe('C')
  })
})
