import { describe, expect, it } from 'vitest'
import { render, screen } from '@testing-library/react'
import IterationConvergenceChart, {
  extractIterationTrail,
  parseIterateResult,
} from './IterationConvergenceChart'

describe('IterationConvergenceChart', () => {
  const trail = [
    { iter: 1, metric: 0.42 },
    { iter: 2, metric: 0.31 },
    { iter: 3, metric: 0.18 },
    { iter: 4, metric: 0.09 },
  ]

  it('renders an SVG with one polyline + per-iteration markers', () => {
    const { container } = render(<IterationConvergenceChart trail={trail} />)
    expect(container.querySelector('svg')).not.toBeNull()
    expect(container.querySelector('[data-trail="true"]')).not.toBeNull()
    const points = container.querySelectorAll('[data-iter-point]')
    expect(points.length).toBe(4)
  })

  it('renders a threshold reference line when provided', () => {
    const { container } = render(
      <IterationConvergenceChart trail={trail} threshold={0.1} operator="<" />,
    )
    const line = container.querySelector('[data-threshold-line="true"]')
    expect(line).not.toBeNull()
    expect(line?.textContent).toContain('<')
    expect(line?.textContent).toContain('0.10')
  })

  it('marks the converged iteration with a filled circle', () => {
    const { container } = render(
      <IterationConvergenceChart trail={trail} convergedAtIter={4} />,
    )
    expect(container.querySelector('[data-converged-marker="true"]')).not.toBeNull()
  })

  it('falls back gracefully when trail collapses to a single value', () => {
    // Same metric every iter — degenerate y-range; chart must still render.
    const flat = [
      { iter: 1, metric: 0.5 },
      { iter: 2, metric: 0.5 },
      { iter: 3, metric: 0.5 },
    ]
    const { container } = render(<IterationConvergenceChart trail={flat} />)
    const points = container.querySelectorAll('[data-iter-point]')
    expect(points.length).toBe(3)
  })

  it('renders nothing when trail is empty', () => {
    const { container } = render(<IterationConvergenceChart trail={[]} />)
    expect(container.querySelector('svg')).toBeNull()
  })

  it('exposes a human-readable summary line for screen readers', () => {
    render(<IterationConvergenceChart trail={trail} convergedAtIter={4} />)
    expect(screen.getByText(/Converged at iter 4/i)).toBeInTheDocument()
  })

  it('falls back to "N iterations" summary when not converged', () => {
    render(<IterationConvergenceChart trail={trail} />)
    expect(screen.getByText(/4 iterations; last metric/i)).toBeInTheDocument()
  })
})

describe('extractIterationTrail', () => {
  it('parses canonical [{iter, metric}] shape', () => {
    const out = extractIterationTrail({
      metric_trail: [
        { iter: 1, metric: 0.4 },
        { iter: 2, metric: 0.2 },
      ],
    })
    expect(out).toEqual([
      { iter: 1, metric: 0.4 },
      { iter: 2, metric: 0.2 },
    ])
  })

  it('parses [{iteration, value}] alias shape', () => {
    const out = extractIterationTrail({
      metric_trail: [
        { iteration: 1, value: 0.3 },
        { iteration: 2, value: 0.1 },
      ],
    })
    expect(out).toEqual([
      { iter: 1, metric: 0.3 },
      { iter: 2, metric: 0.1 },
    ])
  })

  it('parses tuple [iter, metric] shape', () => {
    const out = extractIterationTrail({
      metric_trail: [
        [1, 0.4],
        [2, 0.2],
      ],
    })
    expect(out).toEqual([
      { iter: 1, metric: 0.4 },
      { iter: 2, metric: 0.2 },
    ])
  })

  it('sorts out-of-order points by iter', () => {
    const out = extractIterationTrail({
      metric_trail: [
        { iter: 3, metric: 0.1 },
        { iter: 1, metric: 0.5 },
        { iter: 2, metric: 0.3 },
      ],
    })
    expect(out?.map((p) => p.iter)).toEqual([1, 2, 3])
  })

  it('rejects non-finite values', () => {
    const out = extractIterationTrail({
      metric_trail: [
        { iter: 1, metric: 0.5 },
        { iter: 'two', metric: 0.3 },
        { iter: 3, metric: NaN },
      ],
    })
    // The tolerant filter retains the valid (1, 0.5) entry.
    expect(out).toEqual([{ iter: 1, metric: 0.5 }])
  })

  it('returns null when metric_trail is missing', () => {
    expect(extractIterationTrail({ iter_count: 5 })).toBeNull()
  })

  it('returns null when result is not an object', () => {
    expect(extractIterationTrail(null)).toBeNull()
    expect(extractIterationTrail(42)).toBeNull()
  })
})

describe('parseIterateResult', () => {
  it('reads threshold + operator + converged_at alongside the trail', () => {
    const out = parseIterateResult({
      metric_trail: [
        { iter: 1, metric: 0.5 },
        { iter: 2, metric: 0.05 },
      ],
      threshold: 0.1,
      operator: '<',
      converged_at: 2,
    })
    expect(out?.threshold).toBe(0.1)
    expect(out?.operator).toBe('<')
    expect(out?.convergedAtIter).toBe(2)
  })

  it('falls back to converged_at_iter alias', () => {
    const out = parseIterateResult({
      metric_trail: [{ iter: 1, metric: 0.05 }],
      converged_at_iter: 1,
    })
    expect(out?.convergedAtIter).toBe(1)
  })

  it('drops malformed operator strings', () => {
    const out = parseIterateResult({
      metric_trail: [{ iter: 1, metric: 0.05 }],
      operator: 'approximately',
    })
    expect(out?.operator).toBeUndefined()
  })

  it('returns null when the trail is missing', () => {
    expect(parseIterateResult({ iter_count: 5 })).toBeNull()
  })
})
