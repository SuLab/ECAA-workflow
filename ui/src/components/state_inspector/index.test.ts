import { describe, expect, it } from 'vitest'
import { TABS, type Tab } from './index'

describe('TABS registry', () => {
  it('exposes every declared Tab variant exactly once in the registry', () => {
    // The union is closed — this array is the full set expected to
    // appear in the registry. Adding a new Tab variant makes `tsc`
    // fail if the constant isn't extended; the test below is the
    // runtime catch for any accidental duplicates or gaps.
    //
    // `composition`, `verifier_decisions`, and `repairs` were added
    // incrementally; the exhaustive list below is the current closed set.
    const expected: Tab[] = [
      'plan',
      'composition',
      'state',
      'documents',
      'inputs',
      'jobs',
      'metrics',
      'figures',
      'dashboard',
      'decisions',
      'repairs',
      'claims',
      'verifier_decisions',
      'history',
      'compare',
    ]
    const ids = TABS.map((t) => t.id)
    for (const t of expected) {
      expect(ids.filter((id) => id === t)).toHaveLength(1)
    }
    expect(ids).toHaveLength(expected.length)
    // Every entry must carry a non-empty label.
    for (const t of TABS) {
      expect(t.label.length).toBeGreaterThan(0)
    }
  })
})
