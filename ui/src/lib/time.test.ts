import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { act, render } from '@testing-library/react'
import { createElement } from 'react'
import {
  etaFromHistory,
  formatDuration,
  relativeTime,
  useRelativeTime,
} from './time'

describe('relativeTime', () => {
  const now = new Date('2026-04-23T12:00:00Z').getTime()

  it('returns "just now" for < 30s', () => {
    const iso = new Date(now - 5_000).toISOString()
    expect(relativeTime(iso, now)).toBe('just now')
  })

  it('returns minutes for < 60m', () => {
    const iso = new Date(now - 2 * 60_000).toISOString()
    expect(relativeTime(iso, now)).toBe('2 minutes ago')
  })

  it('pluralises minutes correctly', () => {
    const iso = new Date(now - 60_000).toISOString()
    expect(relativeTime(iso, now)).toBe('1 minute ago')
  })

  it('returns hours for < 24h', () => {
    const iso = new Date(now - 3 * 3_600_000).toISOString()
    expect(relativeTime(iso, now)).toBe('3 hours ago')
  })

  it('returns "yesterday at HH:MM" for ~24h ago', () => {
    const iso = new Date(now - 26 * 3_600_000).toISOString()
    const out = relativeTime(iso, now)
    expect(out).toMatch(/^yesterday at \d\d:\d\d$/)
  })

  it('falls back to absolute for > 7 days', () => {
    const iso = new Date(now - 8 * 86_400_000).toISOString()
    const out = relativeTime(iso, now)
    expect(out).toMatch(/at \d\d:\d\d/)
  })

  it('returns "—" on garbage input', () => {
    expect(relativeTime('not-a-date', now)).toBe('—')
  })

  it('handles future timestamps', () => {
    const iso = new Date(now + 5 * 60_000).toISOString()
    expect(relativeTime(iso, now)).toBe('in 5 minutes')
  })
})

describe('formatDuration', () => {
  it('renders seconds', () => {
    expect(formatDuration(45)).toBe('45s')
  })
  it('renders minutes', () => {
    expect(formatDuration(120)).toBe('2 min')
  })
  it('renders h+m', () => {
    expect(formatDuration(3600 * 2 + 600)).toBe('2h 10m')
  })
  it('renders round hours', () => {
    expect(formatDuration(3600 * 3)).toBe('3h')
  })
})

describe('etaFromHistory', () => {
  const now = new Date('2026-04-23T12:00:00Z').getTime()
  const startedAt = new Date(now - 60_000).toISOString()

  it('returns null with fewer than 2 priors', () => {
    expect(
      etaFromHistory(startedAt, 'alignment', [
        { stage_class: 'alignment', elapsed_secs: 600 },
      ], now),
    ).toBeNull()
  })

  it('returns null when no stage-class matches', () => {
    expect(
      etaFromHistory(startedAt, 'alignment', [
        { stage_class: 'qc', elapsed_secs: 120 },
        { stage_class: 'qc', elapsed_secs: 180 },
      ], now),
    ).toBeNull()
  })

  it('computes median - elapsed on tight histories', () => {
    const res = etaFromHistory(
      startedAt,
      'alignment',
      [
        { stage_class: 'alignment', elapsed_secs: 600 },
        { stage_class: 'alignment', elapsed_secs: 600 },
        { stage_class: 'alignment', elapsed_secs: 600 },
      ],
      now,
    )
    // median 600s, elapsed ~60s → eta 540s = 9 min.
    expect(res).not.toBeNull()
    expect(res!.eta_mins).toBe(9)
  })

  it('returns null without startedAt', () => {
    expect(
      etaFromHistory(null, 'alignment', [
        { stage_class: 'alignment', elapsed_secs: 600 },
        { stage_class: 'alignment', elapsed_secs: 600 },
      ], now),
    ).toBeNull()
  })
})

describe('useRelativeTime', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-04-23T12:00:00Z'))
  })
  afterEach(() => {
    vi.useRealTimers()
  })

  function Probe({ iso }: { iso: string }) {
    const label = useRelativeTime(iso)
    return createElement('span', { 'data-testid': 'probe' }, label)
  }

  it('re-renders after the 60s tick', () => {
    // Start with 10s old so "just now". Advance 2 minutes inside
    // `act()` so the 60s interval fires and the relative string
    // ticks up.
    const iso = new Date(Date.now() - 10_000).toISOString()
    const { getByTestId } = render(createElement(Probe, { iso }))
    expect(getByTestId('probe').textContent).toBe('just now')
    act(() => {
      vi.advanceTimersByTime(120_000)
    })
    expect(getByTestId('probe').textContent).toMatch(/minute/)
  })
})
