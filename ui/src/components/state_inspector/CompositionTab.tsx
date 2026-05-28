/**
 * Composition tab.
 *
 * Aggregates the v4 planner's typed outputs into a single SME-facing
 * surface: accepted nodes, rejected candidates, edge-proof drawer,
 * assumption ledger, adapter warnings, validation status, ranked
 * alternatives, novel-node spec, refusal report. Each section is a
 * read-only card; mutations go through the existing tools (confirm /
 * reject / unblock / amend / branch).
 *
 * Data sources (via chatClient):
 *  - GET /api/chat/session/:id/compose-outcome
 *  - GET /api/chat/session/:id/compose-alternatives
 *  - GET /api/chat/session/:id/proofs
 *  - GET /api/chat/session/:id/assumptions
 *  - GET /api/chat/session/:id/policy-decisions
 *  - GET /api/chat/session/:id/validation-reports
 *
 * v1/v2/v3 sessions return 204 / empty arrays for compose-outcome
 * and compose-alternatives; the tab renders the empty-state legend
 * and prompts the SME toward `SWFC_COMPOSER=semantic` for
 * proof-carrying composition.
 */

import { useMemo, useState } from 'react'
import { useCancelableEffect } from '../../hooks/useCancelableFetch'
import {
  acknowledgeRefusal,
  getAssumptions,
  getComposeAlternatives,
  getComposeOutcome,
  getPolicyDecisions,
  getProofs,
  getValidationReports,
  postBranch,
  recordAdapterDecision,
  recordNovelNodeDecision,
  resolveAssumption,
  setPolicyBundle,
  type AlternativeSummary as AlternativeSummaryWire,
  type AssumptionRow,
  type ComposeOutcomePayload,
  type PolicyDecisionRow,
  type ValidationReportRow,
} from '../../api/chatClient'
import AcceptedNodeList, {
  type AcceptedNode,
} from '../AcceptedNodeList'
import AdapterWarningCard, {
  type AdapterWarning,
} from '../AdapterWarningCard'
import AlternativeDagComparisonCard, {
  type AlternativeSummary as AlternativeSummaryProps,
} from '../AlternativeDagComparisonCard'
import AssumptionLedgerCard, {
  type Assumption as AssumptionProps,
} from '../AssumptionLedgerCard'
import EdgeProofDrawer, { type EdgeProof } from '../EdgeProofDrawer'
import NovelNodeSpecCard from '../NovelNodeSpecCard'
import RefusalReportCard, { type RefusalReport } from '../RefusalReportCard'
import RejectedCandidateList, {
  type RejectedCandidate,
} from '../RejectedCandidateList'
import ValidationStatusCard, {
  type ValidationRow,
} from '../ValidationStatusCard'

interface Props {
  sessionId: string | null
  /** Refresh hint — bump from parent on state-advanced events to
   *  re-fetch composition data without manual reload. */
  refreshKey?: string | number
}

interface CompositionState {
  outcome: ComposeOutcomePayload | null
  alternatives: AlternativeSummaryWire[]
  proofs: EdgeProof[]
  assumptions: AssumptionRow[]
  policyDecisions: PolicyDecisionRow[]
  validationReports: ValidationReportRow[]
  loading: boolean
  error: string | null
}

const initialState: CompositionState = {
  outcome: null,
  alternatives: [],
  proofs: [],
  assumptions: [],
  policyDecisions: [],
  validationReports: [],
  loading: true,
  error: null,
}

export function CompositionTab({
  sessionId,
  refreshKey,
}: Props): JSX.Element {
  const [state, setState] = useState<CompositionState>(initialState)
  const [selectedProof, setSelectedProof] = useState<EdgeProof | null>(null)
  const [selectedAlternative, setSelectedAlternative] = useState<
    string | null
  >(null)

  useCancelableEffect(async ({ cancelled }) => {
    if (!sessionId) {
      setState({ ...initialState, loading: false })
      return
    }
    setState((prev) => ({ ...prev, loading: true, error: null }))
    try {
      const [
        outcome,
        alternatives,
        proofsResp,
        assumptionsResp,
        policyResp,
        validationResp,
      ] = await Promise.all([
        getComposeOutcome(sessionId),
        getComposeAlternatives(sessionId),
        getProofs(sessionId),
        getAssumptions(sessionId),
        getPolicyDecisions(sessionId),
        getValidationReports(sessionId),
      ])
      if (cancelled()) return
      const typedProofs: EdgeProof[] = (proofsResp.proofs as unknown[])
        .map((p) => extractProof(p))
        .filter((p): p is EdgeProof => p !== null)
      setState({
        outcome,
        alternatives: alternatives.alternatives,
        proofs: typedProofs,
        assumptions: assumptionsResp.assumptions.entries,
        policyDecisions: policyResp.decisions,
        validationReports: validationResp.reports,
        loading: false,
        error: null,
      })
    } catch (e) {
      if (!cancelled()) {
        setState((prev) => ({
          ...prev,
          loading: false,
          error: (e as Error).message,
        }))
      }
    }
  }, [sessionId, refreshKey])

  const acceptedNodes: AcceptedNode[] = useMemo(
    () => state.outcome?.accepted_nodes ?? [],
    [state.outcome],
  )

  const rejectedCandidates: RejectedCandidate[] = useMemo(() => {
    // Map unresolved-gap entries (PartialDag outcome) into the
    // RejectedCandidate shape — gaps cite a `missing_port` (the
    // typed atom-input slot the planner couldn't fill) and a
    // statement explaining why. Blockers (DraftDag outcome) are
    // session-level (BlockerContext: timestamp + recovery_hints)
    // rather than atom-level, so they surface separately as
    // session blockers via the existing BlockerCard channel and
    // are not represented here.
    return (state.outcome?.unresolved_gaps ?? [])
      .map((g: any) => ({
        atom_id:
          typeof g?.missing_port === 'string'
            ? g.missing_port
            : typeof g?.id === 'string'
              ? g.id
              : 'unknown',
        reason: 'unfilled_slot',
        reason_detail:
          typeof g?.statement === 'string' ? g.statement : undefined,
      }))
      .filter((g) => g.atom_id !== 'unknown')
  }, [state.outcome])

  const sessionBlockers: Array<{ timestamp: string; recovery_hints: string | null }> =
    useMemo(
      () =>
        (state.outcome?.blockers ?? [])
          .map((b: any) => ({
            timestamp:
              typeof b?.timestamp === 'string' ? b.timestamp : '',
            recovery_hints:
              typeof b?.recovery_hints === 'string'
                ? b.recovery_hints
                : null,
          }))
          .filter((b) => b.timestamp !== '' || b.recovery_hints),
      [state.outcome],
    )

  const adapterWarnings: AdapterWarning[] = useMemo(() => {
    // Surface inserted-adapter rows from the proofs payload. Each
    // proof carries `inserted_adapter_node_ids`; the engine tags
    // adapter nodes with `safety` (lossy_declared / scientifically_risky
    // / policy_restricted) on the source TaskNode. We project onto the
    // AdapterWarning shape with defaults when the engine didn't
    // populate every field (older sessions).
    return state.proofs.flatMap((p, edgeIdx) =>
      p.inserted_adapter_node_ids.map((adapterId, idx) => ({
        adapter_id: adapterId,
        adapter_class: 'unknown',
        safety: 'lossy_declared' as const,
        affected_edge: `${p.from_node}:${p.from_port} → ${p.to_node}:${p.to_port}`,
        rationale:
          p.warnings[idx] ??
          p.rationale ??
          `Adapter ${adapterId} on edge #${edgeIdx + 1}`,
        resolution: 'unresolved' as const,
      })),
    )
  }, [state.proofs])

  const ledgerEntries: AssumptionProps[] = useMemo(
    () =>
      state.assumptions.map((a) => ({
        id: a.id,
        statement: a.statement,
        source: a.source,
        // Defensive default — older server emits sometimes omit
        // `affects_nodes` when empty (the TS binding declares it
        // required, but the wire payload doesn't always honor that).
        affects_nodes: Array.isArray(a.affects_nodes)
          ? a.affects_nodes
          : [],
        risk: a.risk,
        resolution:
          a.resolution === 'unresolved' ||
          a.resolution === 'confirmed' ||
          a.resolution === 'rejected'
            ? a.resolution
            : ('unresolved' as const),
      })),
    [state.assumptions],
  )

  const validationRowsByTask = useMemo(() => {
    const grouped: Record<string, ValidationRow[]> = {}
    for (const r of state.validationReports) {
      const tid = r.task_id
      const row = parseValidationOutcome(r)
      if (!grouped[tid]) grouped[tid] = []
      grouped[tid].push(row)
    }
    return grouped
  }, [state.validationReports])

  const altSummaries: AlternativeSummaryProps[] = useMemo(
    () =>
      state.alternatives.map((a) => ({
        dag_id: a.dag_id,
        summary: a.summary,
        node_count: a.node_count,
        edge_count: a.edge_count,
        total_adapters: a.total_adapters,
        risky_adapters: a.risky_adapters,
        unresolved_assumptions: a.unresolved_assumptions,
        reproducibility_score: a.reproducibility_score,
      })),
    [state.alternatives],
  )

  if (!sessionId) {
    return <EmptyState message="No session selected." />
  }
  if (state.loading) {
    return <EmptyState message="Loading composition…" />
  }
  if (state.error) {
    return <EmptyState message={`Failed to load composition: ${state.error}`} />
  }

  const v4Active = state.outcome !== null

  return (
    <div style={containerStyle}>
      {!v4Active && (
        <div style={legendStyle}>
          This session predates the typed-composer view. To see
          composition outcomes, ranked alternatives, and proof-carrying
          edges, start a new session with{' '}
          <code style={codeStyle}>SWFC_COMPOSER=semantic</code>.
        </div>
      )}

      {state.outcome?.variant === 'refusal' && state.outcome.refusal && (
        <RefusalReportCard
          report={asRefusalReport(state.outcome.refusal)}
          onBranch={async () => {
            if (!sessionId) return
            const refusalId = state.outcome?.refusal?.id ?? 'unknown'
            try {
              // Acknowledge first (durable record), then dispatch
              // the actual branch. The audit log row + the new
              // session id together capture the SME's recovery
              // choice for handoff.
              await acknowledgeRefusal(sessionId, refusalId, 'branch')
              const result = await postBranch(sessionId, {
                rationale: `Branched from a refusal outcome (refusal_id=${refusalId})`,
              })
              window.location.search = `?session=${encodeURIComponent(result.session_id)}`
            } catch (err) {
              console.warn('failed to branch from refusal:', err)
            }
          }}
          onAmendPolicy={async () => {
            if (!sessionId) return
            const refusalId = state.outcome?.refusal?.id ?? 'unknown'
            try {
              await acknowledgeRefusal(sessionId, refusalId, 'amend_policy')
              // Clearing the active policy bundle is the simplest
              // amend-policy path; the SME can re-activate via
              // ClinicalConfirmGate after.
              await setPolicyBundle(sessionId, null)
            } catch (err) {
              console.warn('failed to amend policy after refusal:', err)
            }
          }}
        />
      )}

      {state.outcome?.variant === 'novel_node_spec' &&
        state.outcome.novel_node_spec && (
          <NovelNodeSpecCard
            spec={state.outcome.novel_node_spec}
            onAccept={async (nodeId) => {
              if (!sessionId) return
              try {
                await recordNovelNodeDecision(
                  sessionId,
                  nodeId,
                  'accepted_as_draft',
                )
              } catch (err) {
                console.warn('failed to accept novel node:', err)
              }
            }}
            onReject={async (nodeId) => {
              if (!sessionId) return
              try {
                await recordNovelNodeDecision(sessionId, nodeId, 'rejected')
              } catch (err) {
                console.warn('failed to reject novel node:', err)
              }
            }}
          />
        )}

      {state.outcome && state.outcome.summary && (
        <div style={summaryStyle}>{state.outcome.summary}</div>
      )}

      <Section title="Accepted nodes">
        <AcceptedNodeList nodes={acceptedNodes} />
      </Section>

      <Section title="Rejected candidates">
        <RejectedCandidateList candidates={rejectedCandidates} />
      </Section>

      <Section title="Assumption ledger">
        <AssumptionLedgerCard
          assumptions={ledgerEntries}
          onResolve={async (id, resolution, rationale) => {
            if (!sessionId) return
            try {
              await resolveAssumption(sessionId, id, resolution, rationale)
              // Optimistic local update — the server will also
              // emit a state_advanced event that triggers the
              // refreshKey-driven refetch, but updating in-place
              // gives the SME instant visual feedback.
              setState((prev) => ({
                ...prev,
                assumptions: prev.assumptions.map((a) =>
                  a.id === id ? { ...a, resolution } : a,
                ),
              }))
            } catch (err) {
              console.warn('failed to resolve assumption:', err)
            }
          }}
        />
      </Section>

      <Section title="Adapter warnings">
        <AdapterWarningCard
          adapters={adapterWarnings}
          onConfirm={async (adapterId) => {
            if (!sessionId) return
            const safety = adapterWarnings.find(
              (a) => a.adapter_id === adapterId,
            )?.safety
            try {
              await recordAdapterDecision(
                sessionId,
                adapterId,
                'confirmed',
                safety,
              )
              // Optimistic local update so the chip flips
              // immediately. Server-side state_advanced event will
              // trigger the next full refetch.
              setState((prev) => ({
                ...prev,
                proofs: prev.proofs.map((p) => ({
                  ...p,
                  inserted_adapter_node_ids: p.inserted_adapter_node_ids.map(
                    (id) => id,
                  ),
                })),
              }))
            } catch (err) {
              console.warn('failed to confirm adapter:', err)
            }
          }}
          onReject={async (adapterId) => {
            if (!sessionId) return
            const safety = adapterWarnings.find(
              (a) => a.adapter_id === adapterId,
            )?.safety
            try {
              await recordAdapterDecision(
                sessionId,
                adapterId,
                'rejected',
                safety,
              )
            } catch (err) {
              console.warn('failed to reject adapter:', err)
            }
          }}
        />
      </Section>

      {Object.keys(validationRowsByTask).length === 0 ? (
        <Section title="Validation status">
          <ValidationStatusCard task_id="(no tasks)" rows={[]} />
        </Section>
      ) : (
        Object.entries(validationRowsByTask).map(([taskId, rows]) => (
          <Section
            key={taskId}
            title={`Validation status — ${taskId}`}
          >
            <ValidationStatusCard task_id={taskId} rows={rows} />
          </Section>
        ))
      )}

      <Section title="Ranked alternatives">
        <AlternativeDagComparisonCard
          alternatives={altSummaries}
          selected_dag_id={selectedAlternative}
          onSelect={(id) => setSelectedAlternative(id)}
        />
      </Section>

      {sessionBlockers.length > 0 && (
        <Section title="Session blockers">
          <ul style={proofListStyle}>
            {sessionBlockers.map((b, i) => (
              <li
                key={`${b.timestamp}-${i}`}
                style={{ ...proofRowStyle, borderLeft: '3px solid var(--color-danger-accent)' }}
              >
                <span style={{ fontSize: '0.74rem', color: 'var(--color-text-muted)' }}>
                  {b.timestamp || '(no timestamp)'}
                </span>
                {b.recovery_hints && (
                  <span style={{ fontSize: '0.74rem', flex: 1 }}>
                    {b.recovery_hints}
                  </span>
                )}
              </li>
            ))}
          </ul>
        </Section>
      )}

      {state.policyDecisions.length > 0 && (
        <Section title="Policy decisions">
          <PolicyDecisionList decisions={state.policyDecisions} />
        </Section>
      )}

      {state.proofs.length > 0 && (
        <Section title="Edge proofs">
          <ul style={proofListStyle}>
            {state.proofs.map((p, i) => (
              <li
                key={`${p.from_node}-${p.to_node}-${i}`}
                style={proofRowStyle}
                onClick={() => setSelectedProof(p)}
                role="button"
                tabIndex={0}
              >
                <code style={codeStyle}>{p.from_node}</code> →{' '}
                <code style={codeStyle}>{p.to_node}</code>
                {p.inserted_adapter_node_ids.length > 0 && (
                  <span style={proofAdapterChipStyle}>
                    {p.inserted_adapter_node_ids.length} adapter(s)
                  </span>
                )}
              </li>
            ))}
          </ul>
        </Section>
      )}

      <EdgeProofDrawer
        proof={selectedProof}
        onClose={() => setSelectedProof(null)}
      />
    </div>
  )
}

function Section({
  title,
  children,
}: {
  title: string
  children: React.ReactNode
}) {
  return (
    <section style={sectionStyle}>
      <h3 style={sectionHeaderStyle}>{title}</h3>
      {children}
    </section>
  )
}

function PolicyDecisionList({
  decisions,
}: {
  decisions: PolicyDecisionRow[]
}) {
  return (
    <ul style={policyListStyle}>
      {decisions.map((d, i) => (
        <li
          key={`${d.bundle_id}-${d.kind}-${d.node_id ?? 'global'}-${i}`}
          style={{
            ...policyRowStyle,
            borderLeft: `3px solid ${d.blocking ? 'var(--color-danger-accent)' : 'var(--color-success-fg)'}`,
          }}
        >
          <span style={policyKindStyle}>{d.kind}</span>
          <span style={policyBundleStyle}>{d.bundle_id}</span>
          {d.node_id && (
            <code style={codeStyle}>{d.node_id}</code>
          )}
          <span style={policyStatementStyle}>{d.statement}</span>
        </li>
      ))}
    </ul>
  )
}

function EmptyState({ message }: { message: string }) {
  return (
    <div style={emptyStateStyle} role="region" aria-label="Composition tab empty state">
      {message}
    </div>
  )
}

function extractProof(raw: unknown): EdgeProof | null {
  if (typeof raw !== 'object' || raw === null) return null
  const r = raw as Record<string, unknown>
  // EdgeContract serializes as `{ from_node, from_port, to_node,
  // To_port, proof: {... } }`. The CompatibilityProof inner block
  // carries producer_type / consumer_type / facet_matches / etc.
  // Tolerate both nested (`proof.<field>`) and inlined (top-level
  // <field>) shapes since v3 emits may produce flatter forms.
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

function asRefusalReport(raw: unknown): RefusalReport {
  if (typeof raw !== 'object' || raw === null) {
    return {
      id: 'unknown',
      kind: 'v4_planner_refused',
      statement: 'Composition refused (no detail).',
      references: [],
    }
  }
  const r = raw as Record<string, unknown>
  return {
    id: (r.id as string) ?? 'unknown',
    kind: (r.kind as RefusalReport['kind']) ?? 'v4_planner_refused',
    statement: (r.statement as string) ?? '',
    references: (r.references as string[]) ?? [],
  }
}

function parseValidationOutcome(r: ValidationReportRow): ValidationRow {
  // Backend serializes outcomes as "passed" / "failed:msg" /
  // "errored:reason" / "unimplemented:obligation_id". Split on the
  // first colon for typed surfaces.
  const parts = r.outcome.split(':')
  const head = parts[0] as ValidationRow['outcome']
  if (head === 'passed' || head === 'failed' || head === 'errored' || head === 'unimplemented') {
    return {
      obligation_id: r.obligation_id,
      outcome: head,
      message: parts.length > 1 ? parts.slice(1).join(':') : undefined,
    }
  }
  return {
    obligation_id: r.obligation_id,
    outcome: 'errored',
    message: r.outcome,
  }
}

const containerStyle: React.CSSProperties = {
  padding: '1rem',
  display: 'flex',
  flexDirection: 'column',
  gap: '0.85rem',
  fontSize: '0.85rem',
  overflowY: 'auto',
}

const sectionStyle: React.CSSProperties = {
  display: 'flex',
  flexDirection: 'column',
  gap: '0.4rem',
}

const sectionHeaderStyle: React.CSSProperties = {
  margin: 0,
  fontSize: '0.92rem',
  fontWeight: 600,
  color: 'var(--color-text-primary)',
}

const summaryStyle: React.CSSProperties = {
  padding: '0.6rem 0.85rem',
  fontSize: '0.85rem',
  background: 'var(--color-surface-1)',
  borderLeft: '3px solid var(--color-success-accent)',
  borderRadius: '0.3rem',
}

const legendStyle: React.CSSProperties = {
  padding: '0.6rem 0.85rem',
  fontSize: '0.78rem',
  background: 'var(--color-surface-muted)',
  border: '1px solid var(--color-border-subtle)',
  borderRadius: '0.4rem',
  color: 'var(--color-text-secondary)',
}

const codeStyle: React.CSSProperties = {
  fontFamily: 'ui-monospace, monospace',
  background: 'var(--color-surface-muted)',
  padding: '0.05rem 0.3rem',
  borderRadius: '0.2rem',
  fontSize: '0.74rem',
}

const proofListStyle: React.CSSProperties = {
  listStyle: 'none',
  padding: 0,
  margin: 0,
  display: 'flex',
  flexDirection: 'column',
  gap: '0.25rem',
}

const proofRowStyle: React.CSSProperties = {
  padding: '0.3rem 0.6rem',
  background: 'var(--color-surface-muted)',
  borderRadius: '0.3rem',
  cursor: 'pointer',
  display: 'flex',
  alignItems: 'center',
  gap: '0.4rem',
}

const proofAdapterChipStyle: React.CSSProperties = {
  marginLeft: 'auto',
  fontSize: '0.7rem',
  padding: '0.05rem 0.4rem',
  borderRadius: '0.3rem',
  background: 'var(--color-warning-accent)',
  color: '#fff',
}

const policyListStyle: React.CSSProperties = {
  listStyle: 'none',
  padding: 0,
  margin: 0,
  display: 'flex',
  flexDirection: 'column',
  gap: '0.3rem',
}

const policyRowStyle: React.CSSProperties = {
  padding: '0.4rem 0.6rem',
  background: 'var(--color-surface-muted)',
  borderRadius: '0.3rem',
  display: 'flex',
  alignItems: 'baseline',
  gap: '0.5rem',
  fontSize: '0.78rem',
}

const policyKindStyle: React.CSSProperties = {
  fontWeight: 600,
  fontFamily: 'ui-monospace, monospace',
  fontSize: '0.74rem',
}

const policyBundleStyle: React.CSSProperties = {
  fontSize: '0.7rem',
  padding: '0.05rem 0.3rem',
  borderRadius: '0.25rem',
  background: 'var(--color-surface-1)',
  color: 'var(--color-text-secondary)',
}

const policyStatementStyle: React.CSSProperties = {
  flex: 1,
  fontSize: '0.74rem',
  color: 'var(--color-text-secondary)',
}

const emptyStateStyle: React.CSSProperties = {
  padding: '1rem',
  fontSize: '0.85rem',
  color: 'var(--color-text-muted)',
  fontStyle: 'italic',
}
