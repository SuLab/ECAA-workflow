import { useEffect, useState } from 'react'
import { useCancelableEffect } from '../../hooks/useCancelableFetch'
import type { DAG } from '../../types'
import type { RefusalReport } from '../../types/RefusalReport'
import DagCanvas from '../DagCanvas'
import DagFilterChips, { loadFilter } from '../DagFilterChips'
import EdgeProofDrawer, { type EdgeProof } from '../EdgeProofDrawer'
import ProgressBar from '../ProgressBar'
import TaskDetailDrawer from '../TaskDetailDrawer'
import { getComposeOutcome, getProofs } from '../../api/chatClient'
import { PlaceholderPane } from './common'
import { PromotionRefusedCard } from './PromotionRefusedCard'

interface Props {
  dag: DAG | null
  sessionId: string | null
  /** Non-null when this session was branched from another via
   *  `branch_session`; drives the parent-link chip. */
  parentSessionId?: string | null
}

/**
 * Renders the ProgressBar + DagCanvas when the session has a DAG;
 * otherwise a placeholder asks for more intake context. Clicking a
 * node opens a TaskDetailDrawer that slides in from the right with
 * the full stage brief + action affordances.
 */
export function PlanTab({ dag, sessionId, parentSessionId }: Props): JSX.Element {
  const [activeTaskId, setActiveTaskId] = useState<string | null>(null)
  const [statusFilter, setStatusFilter] = useState<Set<string>>(() => loadFilter())
  // Edge proofs for the EdgeProofDrawer surfaced when the
  // SME clicks an edge in the canvas. Lazy-loaded on first edge
  // click so v1/v2/v3 sessions (which don't emit proofs) don't pay
  // the network cost.
  const [proofs, setProofs] = useState<EdgeProof[] | null>(null)
  const [selectedProof, setSelectedProof] = useState<EdgeProof | null>(null)
  // v3 P3 / v4 P3 (F11/F19) — `PromotionRefused` refusal carries
  // per-node lifecycle-grid failures and the EscalateToReviewer unblock
  // paths that resolve them. Surface a dedicated card above the canvas
  // when the most-recent compose outcome was a promotion refusal.
  const [promotionRefusal, setPromotionRefusal] =
    useState<RefusalReport | null>(null)

  useCancelableEffect(async ({ cancelled }) => {
    if (!sessionId) {
      setPromotionRefusal(null)
      return
    }
    try {
      const outcome = await getComposeOutcome(sessionId)
      if (cancelled()) return
      if (
        outcome &&
        outcome.variant === 'refusal' &&
        outcome.refusal &&
        isPromotionRefused(outcome.refusal)
      ) {
        setPromotionRefusal(outcome.refusal as unknown as RefusalReport)
      } else {
        setPromotionRefusal(null)
      }
    } catch {
      if (!cancelled()) setPromotionRefusal(null)
    }
  }, [sessionId])

  // Support deep-linking to a specific task via `#task=<id>` in the
  // URL. Two producers use this contract:
  // 1. DecisionsTab — sets location.hash when the SME clicks
  // "Open step" on a decision card.
  // 2. NeedsInputChip (app header) — sets location.hash on click to
  // jump the SME to the first blocked task.
  //
  // Both producers mutate location.hash, so we need BOTH an on-mount /
  // on-dag-change read AND a hashchange listener. Without the listener
  // a click on the chip while the Plan tab is already active is a
  // no-op: the hash changes but React doesn't see it.
  useEffect(() => {
    const applyHash = () => {
      const m = window.location.hash.match(/#task=([^&]+)/)
      if (m && dag?.tasks[decodeURIComponent(m[1]!)]) {
        setActiveTaskId(decodeURIComponent(m[1]!))
        // Clear the hash so a page refresh doesn't reopen the drawer,
        // AND so a subsequent click on the same chip still fires the
        // hashchange event (the browser suppresses duplicate-identical
        // hash writes).
        history.replaceState(null, '', window.location.pathname + window.location.search)
      }
    }
    applyHash()
    window.addEventListener('hashchange', applyHash)
    return () => window.removeEventListener('hashchange', applyHash)
  }, [dag])

  if (!dag) {
    return (
      <PlaceholderPane>
        Your plan will appear here once the conversation has enough detail
        to outline the analysis.
      </PlaceholderPane>
    )
  }
  return (
    <>
      {parentSessionId && (
        <div
          role="status"
          style={{
            display: 'flex',
            alignItems: 'center',
            gap: 8,
            padding: '8px 12px',
            background: 'var(--color-warning-bg)',
            borderBottom: '1px solid var(--color-warning-border)',
            fontSize: '0.82rem',
            color: 'var(--color-warning-fg)',
          }}
        >
          <span aria-hidden>⎇</span>
          <span>
            This is a branched session — changes here don't affect the
            original analysis.
          </span>
          <a
            href={`/?session=${encodeURIComponent(parentSessionId)}`}
            style={{
              marginLeft: 'auto',
              color: 'var(--color-warning-accent)',
              textDecoration: 'underline',
              fontWeight: 500,
            }}
          >
            Open parent session
          </a>
        </div>
      )}
      <ProgressBar dag={dag} />
      {promotionRefusal && (
        <div style={{ padding: '8px 12px' }}>
          <PromotionRefusedCard refusal={promotionRefusal} />
        </div>
      )}
      <DagFilterChips selected={statusFilter} onChange={setStatusFilter} />
      <div style={{ flex: 1, minHeight: 0 }}>
        <DagCanvas
          dag={dag}
          activeTaskId={activeTaskId}
          onNodeClick={(id) => setActiveTaskId(id)}
          onEdgeClick={async (fromId, toId) => {
            if (!sessionId) return
            // Lazy-load proofs the first time any edge is clicked.
            // Subsequent clicks reuse the cached list.
            let cached = proofs
            if (cached === null) {
              try {
                const resp = await getProofs(sessionId)
                cached = (resp.proofs as unknown[])
                  .map((p) => extractProofFromRaw(p))
                  .filter((p): p is EdgeProof => p !== null)
                setProofs(cached)
              } catch (err) {
                console.warn('failed to load edge proofs:', err)
                cached = []
                setProofs(cached)
              }
            }
            const match = cached.find(
              (p) => p.from_node === fromId && p.to_node === toId,
            )
            if (match) {
              setSelectedProof(match)
            } else {
              // Synthesize a stub so the drawer still opens with
              // basic edge info — useful for v1/v2/v3 sessions
              // where there's no proof on disk but the SME still
              // wants to see what the edge represents.
              setSelectedProof({
                from_node: fromId,
                from_port: 'output',
                to_node: toId,
                to_port: 'input',
                producer_type: '(no proof recorded)',
                consumer_type: '(no proof recorded)',
                ontology_subsumption_path: [],
                facet_matches: [],
                inserted_adapter_node_ids: [],
                warnings: [
                  'This session is on a legacy composer; per-edge proofs are unavailable. Re-create the session with SWFC_COMPOSER=semantic to capture edge-level compatibility proofs.',
                ],
                rationale: undefined,
              })
            }
          }}
          statusFilter={statusFilter}
        />
      </div>
      <TaskDetailDrawer
        sessionId={sessionId}
        taskId={activeTaskId}
        dag={dag}
        onClose={() => setActiveTaskId(null)}
        onTaskLink={(id) => setActiveTaskId(id)}
      />
      <EdgeProofDrawer
        proof={selectedProof}
        onClose={() => setSelectedProof(null)}
      />
    </>
  )
}

/**
 * v3 P3 / v4 P3 — runtime check for `RefusalKind::PromotionRefused`.
 * The server serializes `ComposeOutcomeResponse.refusal` as
 * `serde_json::to_value(report)`; the typed kind nests under
 * `refusal.kind.kind === "promotion_refused"`. Wrapped here so the
 * PlanTab effect stays readable.
 */
function isPromotionRefused(raw: unknown): boolean {
  if (!raw || typeof raw !== 'object') return false
  const r = raw as Record<string, unknown>
  const kind = r.kind
  if (!kind || typeof kind !== 'object') return false
  return (kind as Record<string, unknown>).kind === 'promotion_refused'
}

/** Flatten an EdgeContract JSON into the EdgeProof shape
 *  the drawer expects. Tolerates both nested (`proof.<field>`) and
 *  inlined shapes. Mirrors `extractProof` in CompositionTab. */
function extractProofFromRaw(raw: unknown): EdgeProof | null {
  if (typeof raw !== 'object' || raw === null) return null
  const r = raw as Record<string, unknown>
  const proofObj =
    (r.proof as Record<string, unknown> | undefined) ??
    (r.compatibility_proof as Record<string, unknown> | undefined) ??
    r
  const fromNode = (r.from_node ?? proofObj.from_node) as string | undefined
  const fromPort = (r.from_port ?? proofObj.from_port) as string | undefined
  const toNode = (r.to_node ?? proofObj.to_node) as string | undefined
  const toPort = (r.to_port ?? proofObj.to_port) as string | undefined
  if (!fromNode || !toNode) return null
  return {
    from_node: fromNode,
    from_port: fromPort ?? 'output',
    to_node: toNode,
    to_port: toPort ?? 'input',
    producer_type: (proofObj.producer_type as string) ?? 'unknown',
    consumer_type: (proofObj.consumer_type as string) ?? 'unknown',
    ontology_subsumption_path:
      (proofObj.ontology_subsumption_path as string[]) ?? [],
    facet_matches: (proofObj.facet_matches as any[]) ?? [],
    inserted_adapter_node_ids:
      (proofObj.inserted_adapter_node_ids as string[]) ?? [],
    warnings: (proofObj.warnings as string[]) ?? [],
    rationale: (proofObj.rationale as string) ?? undefined,
  }
}
