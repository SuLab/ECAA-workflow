// Verifier decision substrate tab — v4 P2 / F18.
//
// Fetches `/api/chat/session/:id/verifier-decisions` and renders a
// filterable table over the typed `VerifierDecision` enum produced by
// the v4 composer + compatibility engine. Read-only; mutations to the
// substrate happen at compose time, not from the UI.
//
// Empty state (no rows / no emit yet / pre-v4 session) shows a
// short explainer so the SME isn't left staring at a blank panel.

import { Fragment, useMemo, useState } from 'react'
import { jsonFetch } from '../../api/_fetch'
import { useCancelableEffect } from '../../hooks/useCancelableFetch'

// `VerifierDecision` is a tagged union with 12 variants — keep the
// table generic over `Record<string, unknown>` so a new ts-rs export
// doesn't require a tab update before the substrate file changes shape.
// The runtime fetches via plain fetch to avoid coupling the chatClient
// to the substrate before ts-rs regen lands.
interface VerifierDecisionRow {
  kind: string
  id?: string
  timestamp?: string
  // Variant-specific payload; rendered as a JSON snippet in the
  // per-row "Details" cell when no first-class column applies.
  [key: string]: unknown
}

interface Props {
  sessionId: string | null
  /** Refresh hint — bump from parent on state-advanced events so the
   *  table re-fetches when a new emit lands. */
  refreshKey?: string | number
}

/** Ordered set of "kind" filters. The "all" sentinel is null. */
const FILTERS: Array<{ id: string | null; label: string }> = [
  { id: null, label: 'All' },
  { id: 'unification_attempted', label: 'Attempted' },
  { id: 'unification_succeeded', label: 'Succeeded' },
  { id: 'unification_failed', label: 'Failed' },
  { id: 'alternative_ranked', label: 'Alternatives' },
  { id: 'assumption_policy_consulted', label: 'Policy' },
  { id: 'promotion_gate_consulted', label: 'Promotion' },
  { id: 'ontology_scope_checked', label: 'Ontology' },
  { id: 'adapter_inserted', label: 'Adapters' },
  { id: 'proposal_rejected', label: 'Rejections' },
  { id: 'repair_proposed', label: 'Repair proposed' },
  { id: 'repair_accepted', label: 'Repair accepted' },
  { id: 'repair_rejected', label: 'Repair rejected' },
]

/**
 * Per-variant primary-field rendering. Each kind picks one or two
 * fields to surface in the table's "Detail" column so the SME can
 * scan rows without expanding every JSON payload.
 */
function renderDetail(row: VerifierDecisionRow): string {
  switch (row.kind) {
    case 'unification_attempted':
    case 'unification_succeeded':
    case 'unification_failed':
      return `${row.producer_port ?? '?'} → ${row.consumer_port ?? '?'}`
    case 'alternative_ranked':
      return `rank=${row.rank} source=${row.source} dag=${row.dag_id}`
    case 'assumption_policy_consulted':
      return `${row.defect_class}/${row.privacy_class} → ${row.resolution}`
    case 'promotion_gate_consulted':
      return `${row.node_id} → ${row.target_state} (${row.result})`
    case 'ontology_scope_checked':
      return `${row.modality} :: ${row.candidate_iri} → ${row.result}`
    case 'adapter_inserted':
      return `${row.adapter_class} (${row.safety}) ${row.producer_node} → ${row.consumer_node}`
    case 'proposal_rejected': {
      const reason = (row.reason as Record<string, unknown> | undefined)?.kind
      return `${row.source}/${row.proposal_kind ?? ''} rejected by ${row.rejected_by}${reason ? ` (${reason})` : ''}`
    }
    case 'repair_proposed':
      return `${row.strategy} for ${row.gap_id} (risk=${row.risk_class})`
    case 'repair_accepted':
      return `${row.proposal_id} by ${row.acceptor}`
    case 'repair_rejected':
      return `${row.proposal_id} (${row.reason})`
    default:
      return ''
  }
}

export function VerifierDecisionsTab({
  sessionId,
  refreshKey,
}: Props): JSX.Element {
  const [rows, setRows] = useState<VerifierDecisionRow[]>([])
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [filter, setFilter] = useState<string | null>(null)
  // Per-row "expand JSON" toggles, keyed by id (falls back to index
  // when id is absent). Plain Set so we don't churn React state on
  // the typical "expand one then collapse" interaction.
  const [expanded, setExpanded] = useState<Set<string>>(new Set())

  useCancelableEffect(async ({ signal, cancelled }) => {
    if (!sessionId) {
      setRows([])
      setError(null)
      return
    }
    setLoading(true)
    setError(null)
    try {
      // Cap at 2000 rows: the v4 substrate can be 100k+ for non-trivial
      // sessions; loading all rows freezes the React render. Server
      // also defaults to 5000 when no limit is passed.
      const json = await jsonFetch<unknown>(
        `/api/chat/session/${encodeURIComponent(sessionId)}/verifier-decisions?limit=2000`,
        { signal },
      )
      if (cancelled()) return
      if (!Array.isArray(json)) {
        throw new Error('expected JSON array')
      }
      setRows(json as VerifierDecisionRow[])
      setLoading(false)
    } catch (e) {
      if (!cancelled()) {
        setError(`Failed to load verifier decisions: ${(e as Error).message}`)
        setLoading(false)
      }
    }
  }, [sessionId, refreshKey])

  const filtered = useMemo(() => {
    if (filter === null) return rows
    return rows.filter((r) => r.kind === filter)
  }, [rows, filter])

  // Per-kind counts for the filter bar so the SME sees substrate
  // composition at a glance.
  const counts = useMemo(() => {
    const m = new Map<string, number>()
    for (const r of rows) m.set(r.kind, (m.get(r.kind) ?? 0) + 1)
    return m
  }, [rows])

  if (!sessionId) {
    return (
      <div style={{ padding: 16, color: '#666' }}>
        Start a session to view verifier decisions.
      </div>
    )
  }
  if (loading) {
    return (
      <div style={{ padding: 16, color: '#666' }}>
        Loading verifier decisions…
      </div>
    )
  }
  if (error) {
    return (
      <div style={{ padding: 16, color: '#b00' }}>{error}</div>
    )
  }

  return (
    <div style={{ padding: 16, fontSize: 13 }}>
      <div style={{ marginBottom: 12 }}>
        <div
          style={{
            fontSize: 14,
            fontWeight: 600,
            marginBottom: 4,
          }}
        >
          Composer trace
        </div>
        <div style={{ color: '#666', fontSize: 12, marginBottom: 8 }}>
          Port-unification search trace from the v4 proof-carrying
          composer. The planner explores many candidate edges; the
          successful unifications form the emitted DAG, and the failures
          are dead-end search branches the type system correctly
          rejected. A high failure rate here is normal and is{' '}
          <strong>not</strong> an indicator of analysis problems — for
          per-task narrative-vs-table claim verification, see the{' '}
          <strong>Claims</strong> tab. Read from{' '}
          <code>runtime/verifier-decisions.jsonl</code>.
        </div>
        <div
          style={{
            display: 'flex',
            flexWrap: 'wrap',
            gap: 6,
            marginBottom: 8,
          }}
        >
          {FILTERS.map((f) => {
            const active = filter === f.id
            const count =
              f.id === null
                ? rows.length
                : counts.get(f.id) ?? 0
            const dim = count === 0 && f.id !== null
            return (
              <button
                key={String(f.id)}
                type="button"
                onClick={() => setFilter(f.id)}
                disabled={dim}
                style={{
                  background: active ? '#1f6feb' : '#eef',
                  color: active ? '#fff' : dim ? '#999' : '#222',
                  border: '1px solid',
                  borderColor: active ? '#1f6feb' : '#ccd',
                  borderRadius: 4,
                  fontSize: 12,
                  padding: '3px 8px',
                  cursor: dim ? 'default' : 'pointer',
                }}
              >
                {f.label} ({count})
              </button>
            )
          })}
        </div>
      </div>
      {filtered.length === 0 ? (
        <div style={{ color: '#666', padding: '32px 0', textAlign: 'center' }}>
          {rows.length === 0
            ? 'No verifier decisions for this session yet — the substrate is written on emit, and is empty for pre-v4 sessions.'
            : 'No decisions match the active filter.'}
        </div>
      ) : (
        <table
          style={{
            width: '100%',
            borderCollapse: 'collapse',
            fontSize: 12,
          }}
        >
          <thead>
            <tr style={{ textAlign: 'left', borderBottom: '1px solid #ddd' }}>
              <th style={{ padding: '6px 8px' }}>Kind</th>
              <th style={{ padding: '6px 8px' }}>Detail</th>
              <th style={{ padding: '6px 8px' }}>Id</th>
              <th style={{ padding: '6px 8px' }}> </th>
            </tr>
          </thead>
          <tbody>
            {filtered.map((row, i) => {
              // `row.id` is not guaranteed unique in the substrate (the same
              // unification pair can appear under multiple contexts), so
              // suffix the row index for a stable, collision-free React key.
              const expandToggleKey = (row.id as string | undefined) ?? `idx-${i}`
              const key = `${expandToggleKey}#${i}`
              const isExpanded = expanded.has(expandToggleKey)
              return (
                <Fragment key={key}>
                  <tr
                    style={{
                      borderBottom: '1px solid #eee',
                      background: i % 2 === 0 ? '#fafbfd' : '#fff',
                    }}
                  >
                    <td
                      style={{ padding: '4px 8px', fontFamily: 'monospace' }}
                    >
                      {row.kind}
                    </td>
                    <td style={{ padding: '4px 8px' }}>{renderDetail(row)}</td>
                    <td
                      style={{
                        padding: '4px 8px',
                        fontFamily: 'monospace',
                        color: '#777',
                      }}
                    >
                      {row.id ? String(row.id) : '—'}
                    </td>
                    <td style={{ padding: '4px 8px' }}>
                      <button
                        type="button"
                        onClick={() => {
                          const next = new Set(expanded)
                          if (isExpanded) next.delete(key)
                          else next.add(key)
                          setExpanded(next)
                        }}
                        style={{
                          background: 'none',
                          border: '1px solid #ccd',
                          borderRadius: 4,
                          fontSize: 11,
                          padding: '1px 6px',
                          cursor: 'pointer',
                        }}
                      >
                        {isExpanded ? 'Hide' : 'JSON'}
                      </button>
                    </td>
                  </tr>
                  {isExpanded && (
                    <tr>
                      <td colSpan={4} style={{ padding: 0 }}>
                        <pre
                          style={{
                            margin: 0,
                            padding: '8px 12px',
                            background: '#f4f5f7',
                            color: '#222',
                            fontSize: 11,
                            overflow: 'auto',
                            whiteSpace: 'pre-wrap',
                          }}
                        >
                          {JSON.stringify(row, null, 2)}
                        </pre>
                      </td>
                    </tr>
                  )}
                </Fragment>
              )
            })}
          </tbody>
        </table>
      )}
    </div>
  )
}
