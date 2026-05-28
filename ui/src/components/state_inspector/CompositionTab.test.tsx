// CompositionTab regression coverage. The tab calls six endpoints in
// parallel and feeds their payloads into sub-cards. Servers older
// than the affects_nodes-always-serialized fix omit
// `Assumption.affects_nodes` whenever the planner didn't populate it,
// and the TypeScript binding's required `string[]` field is then
// `undefined` at runtime — `AssumptionLedgerCard` accesses
// `a.affects_nodes.length` and the whole tab crashes. This test
// locks in the defensive `Array.isArray` projection in the tab
// itself so the UI no longer trusts the wire payload's shape.

import { afterEach, describe, expect, it, vi } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import { CompositionTab } from './CompositionTab'

function mockFetchByPath(map: Record<string, unknown>) {
  const mock = vi.fn(async (url: string) => {
    const path = new URL(url, 'http://localhost').pathname
    const key = Object.keys(map).find((k) => path.endsWith(k))
    if (!key) {
      return new Response('not found', { status: 404 })
    }
    return new Response(JSON.stringify(map[key]), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    })
  })
  ;(globalThis as unknown as { fetch: typeof fetch }).fetch =
    mock as unknown as typeof fetch
  return mock
}

afterEach(() => {
  vi.restoreAllMocks()
})

describe('CompositionTab', () => {
  it('renders without crashing when an assumption is missing affects_nodes', async () => {
    // Reproduces the wire shape the server emits when
    // `skip_serializing_if = Vec::is_empty` is set on
    // `Assumption.affects_nodes`: the field is omitted entirely. The
    // tab must not throw `Cannot read properties of undefined`.
    mockFetchByPath({
      '/compose-outcome': {
        variant: 'validated_executable_dag',
        summary: 'Validated executable DAG — 1 node, 0 edges.',
        node_count: 1,
        edge_count: 0,
        assumption_count: 1,
        accepted_nodes: [],
        unresolved_gaps: [],
        blockers: [],
      },
      '/compose-alternatives': { alternatives: [] },
      '/proofs': { proofs: [] },
      '/assumptions': {
        assumptions: {
          entries: [
            {
              id: 'registry_default:data_acquisition',
              statement: 'Registry-default atom selection',
              source: 'registry_default',
              // affects_nodes intentionally omitted — this is what
              // the server actually sends today when the field is
              // empty.
              risk: 'negligible',
              resolution: 'confirmed',
            },
          ],
        },
      },
      '/policy-decisions': { decisions: [] },
      '/validation-reports': { reports: [] },
    })

    render(<CompositionTab sessionId="s-test" />)

    await waitFor(() => {
      expect(screen.getAllByText(/Accepted nodes/).length).toBeGreaterThan(0)
    })
    // Tab body rendered, no crash, and the assumption row (which
    // accesses .affects_nodes.length internally) survived the
    // undefined-field projection.
    expect(screen.getAllByText(/Assumption ledger/).length).toBeGreaterThan(0)
    // Sanity: the assumption's statement renders, proving the
    // ledger card iterated over the entry.
    expect(
      screen.getByText(/Registry-default atom selection/),
    ).toBeInTheDocument()
  })
})
