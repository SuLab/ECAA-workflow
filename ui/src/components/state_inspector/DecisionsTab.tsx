import { forwardRef, useEffect, useMemo, useState } from 'react'
import { Virtuoso } from 'react-virtuoso'
import { useCancelableEffect } from '../../hooks/useCancelableFetch'
import {
  fetchAdjudicationQueue,
  fetchGraduationCandidates,
  resolveAdjudication,
  annotateGraduationCandidate,
  getDecisions,
  getInstallLog,
  type DecisionsResponse,
  type GraduationCandidatesPayload,
} from '../../api/chatClient'
import { METRICS_POLL_MS } from '../../lib/polling'
import { relativeTime } from '../../lib/time'
import type { DAG, DecisionType } from '../../types'
import type { AdjudicationQueueEntry } from '../../types/AdjudicationQueueEntry'
import ExplainButton from '../ExplainButton'
import LifecycleAdjudicationCard, {
  type AdjudicationQueueEntry as LegacyAdjudicationEntry,
} from './LifecycleAdjudicationCard'
import GraduationCandidateCard from './GraduationCandidateCard'

interface Props {
  sessionId: string | null
  /** Refresh on state changes so newly-written decisions appear without a manual reload. */
  refreshKey?: string | number
  /** Called when the SME clicks a decision card that references a specific task/stage. */
  onJumpToTask?: (taskId: string) => void
  /** Used to resolve stage_class → task_id when a decision references a
   *  stage rather than a specific task. Without this, "Open step" jumps
   *  to a non-existent hash on every amend/rerun decision card. */
  dag?: DAG | null
}

const FILTERS: Array<{ id: string | null; label: string }> = [
  { id: null, label: 'All' },
  { id: 'amend_stage', label: 'Amendments' },
  { id: 'rerun_task', label: 'Reruns' },
  { id: 'branch', label: 'Branches' },
  { id: 'confirm', label: 'Confirmations' },
  { id: 'unblock', label: 'Unblocks' },
]

/**
 * Reverse-chronological timeline of every decision recorded against
 * this session. Cards read in plain English: "At 2:15 pm, you
 * amended the Preprocessing step." Each card that references a
 * task carries a Jump-to button that swaps the Plan tab's active
 * node, so the SME can land on the step in context.
 *
 * Data source: GET /api/chat/session/:id/decisions.
 */
export function DecisionsTab({
  sessionId,
  refreshKey,
  onJumpToTask,
  dag,
}: Props): JSX.Element {
  const [records, setRecords] = useState<DecisionsResponse['decisions']>([])
  const [filter, setFilter] = useState<string | null>(null)
  const [err, setErr] = useState<string | null>(null)
  // v3 P8 — lifecycle-adjudication queue. Surfaces the in-tab card when
  // `Session::adjudication_queue` has open entries. Refreshes on
  // refreshKey (same SME mutation cadence as the decision list) and on
  // resolve.
  const [adjudication, setAdjudication] = useState<AdjudicationQueueEntry[]>([])
  // Install-log surface. Polled on the same
  // cadence as decisions (refreshKey) so newly-installed packages
  // appear without a manual reload. Each row is rendered as
  // `atom_id`, `package`, `registry`, `timestamp`.
  const [installLog, setInstallLog] = useState<Array<Record<string, unknown>>>([])
  // v4 P6 / D4 — LocalExtension graduation candidates. Poll on 4s so
  // the SME sees newly-qualified extensions as they cross the threshold.
  const [graduation, setGraduation] = useState<GraduationCandidatesPayload>({
    thresholds: {
      min_usage_count: 0,
      min_unique_sessions: 0,
      min_success_rate: 0,
    },
    candidates: [],
  })

  useCancelableEffect(async ({ cancelled }) => {
    if (!sessionId) {
      setRecords([])
      return
    }
    try {
      const res = await getDecisions(sessionId, filter ?? undefined)
      if (!cancelled()) {
        // Defensive filter: the agent (claude subprocess) sometimes
        // appends free-form audit entries to runtime/decisions.jsonl
        // (e.g. `{kind:"discovery_auto_pick", task_id:"...",...}`
        // with `kind` at the top level instead of nested under
        // `decision.kind`). DecisionCard's
        // `record.decision.kind` access would crash on those.
        setRecords(
          res.decisions.filter(
            (rec) =>
              rec &&
              rec.decision &&
              typeof rec.decision.kind === 'string',
          ),
        )
        setErr(null)
      }
    } catch (e) {
      if (!cancelled()) setErr((e as Error).message)
    }
  }, [sessionId, filter, refreshKey])

  // v3 P8 — refresh the adjudication queue on the same beat as the
  // decisions list. A separate effect so we don't re-fetch decisions
  // every time the queue changes.
  useCancelableEffect(async ({ cancelled }) => {
    if (!sessionId) {
      setAdjudication([])
      return
    }
    try {
      const next = await fetchAdjudicationQueue(sessionId)
      if (!cancelled()) setAdjudication(next)
    } catch {
      // Silent — the queue is auxiliary; failures shouldn't blow up
      // the main decisions surface.
    }
  }, [sessionId, refreshKey])

  // Poll the install-log endpoint on the
  // same beat as decisions. The endpoint returns 200 + empty entries
  // on missing-file / no-package paths, so any thrown error here is
  // a genuine network/server issue worth swallowing without breaking
  // the rest of the tab.
  useCancelableEffect(async ({ cancelled }) => {
    if (!sessionId) {
      setInstallLog([])
      return
    }
    try {
      const res = await getInstallLog(sessionId)
      if (!cancelled()) setInstallLog(res.entries)
    } catch {
      // Auxiliary surface — swallow; the decisions list is the
      // primary content here.
    }
  }, [sessionId, refreshKey])

  // v4 P6 / D4 — 4s graduation-candidates poll. Same cadence as the
  // repair-proposals hook so the two SME-action surfaces share the
  // same perceived freshness. Gated on document.visibilityState so a
  // backgrounded tab doesn't poll.
  useEffect(() => {
    if (!sessionId) {
      setGraduation({
        thresholds: {
          min_usage_count: 0,
          min_unique_sessions: 0,
          min_success_rate: 0,
        },
        candidates: [],
      })
      return
    }
    let cancelled = false
    const tick = async () => {
      try {
        const next = await fetchGraduationCandidates(sessionId)
        if (!cancelled) setGraduation(next)
      } catch {
        // Silent — auxiliary surface.
      }
    }
    void tick()
    const gatedTick = () => {
      if (document.visibilityState !== 'visible') return
      void tick()
    }
    const handle = window.setInterval(gatedTick, METRICS_POLL_MS)
    return () => {
      cancelled = true
      window.clearInterval(handle)
    }
  }, [sessionId])

  const sorted = useMemo(() => {
    const copy = records.slice()
    copy.sort((a, b) => (a.timestamp < b.timestamp ? 1 : -1))
    return copy
  }, [records])

  if (!sessionId) {
    return (
      <div style={{ padding: 16, color: 'var(--color-text-muted)', fontSize: '0.9rem' }}>
        No session selected.
      </div>
    )
  }

  return (
    <div
      style={{
        padding: '12px 16px',
        display: 'flex',
        flexDirection: 'column',
        gap: 12,
        height: '100%',
        overflowY: 'auto',
      }}
      aria-label="Decision history"
    >
      <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap' }}>
        {FILTERS.map((f) => {
          const active = filter === f.id
          return (
            <button
              key={String(f.id)}
              type="button"
              onClick={() => setFilter(f.id)}
              style={{
                padding: '4px 10px',
                fontSize: '0.78rem',
                borderRadius: 14,
                border: `1px solid ${active ? 'var(--color-accent)' : 'var(--color-border-strong)'}`,
                background: active ? 'var(--color-info-bg)' : 'var(--color-surface-1)',
                color: active ? 'var(--color-info-fg)' : 'var(--color-text-secondary)',
                cursor: 'pointer',
              }}
            >
              {f.label}
            </button>
          )
        })}
      </div>

      {err && (
        <div style={{ fontSize: '0.82rem', color: 'var(--color-danger-fg)' }}>
          Failed to load decisions: {err}
        </div>
      )}

      {adjudication.length > 0 && (
        <LifecycleAdjudicationCard
          entries={adjudication as unknown as LegacyAdjudicationEntry[]}
          onResolve={async (entryId, decidedBy, decision) => {
            if (!sessionId) return
            try {
              await resolveAdjudication(sessionId, entryId, decidedBy, decision)
              const next = await fetchAdjudicationQueue(sessionId)
              setAdjudication(next)
            } catch {
              // Surface failures inline; the card's UX falls through to
              // the next interaction.
            }
          }}
        />
      )}

      {graduation.candidates.length > 0 && (
        <GraduationCandidateCard
          data={graduation}
          onAnnotate={async (iri, annotatedBy, submissionRef, rationale) => {
            if (!sessionId) return
            try {
              await annotateGraduationCandidate(
                sessionId,
                iri,
                annotatedBy,
                submissionRef,
                rationale,
              )
              const next = await fetchGraduationCandidates(sessionId)
              setGraduation(next)
            } catch {
              // Auxiliary surface — swallow.
            }
          }}
        />
      )}

      <InstallLogSection entries={installLog} />

      {sorted.length === 0 && (
        <div style={{ color: 'var(--color-text-muted)', fontSize: '0.88rem' }}>
          No decisions recorded yet. They appear here as soon as the first
          SME action (confirm, amend, rerun, branch) fires.
        </div>
      )}

      {/* C24 virtualization: a long-running session accumulates one
          decision per SME mutation (confirm/amend/rerun/branch/etc.).
          Render through Virtuoso so only the visible window mounts.
          Use `components.List: 'ul'` + `components.Item: 'li'` to keep
          the semantic <ul>/<li> structure assistive tech expects;
          `DecisionCard` now returns a <div> so we don't nest <li>s. */}
      {sorted.length > 0 && (
        <Virtuoso
          data={sorted}
          // Virtuoso's Components type narrows List/Item to
          // ComponentType<HTMLDivElement>; we render semantic <ul>/<li>
          // so a11y trees announce list semantics. Runtime contract is
          // unchanged.
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          components={virtuosoListComponents as any}
          itemContent={(_idx, d) => (
            <DecisionCard
              record={d}
              onJumpToTask={onJumpToTask}
              dag={dag}
            />
          )}
          computeItemKey={(_idx, d) => `${d.timestamp}-${d.decision.kind}`}
          initialItemCount={Math.min(sorted.length, 50)}
          style={{ flex: 1, minHeight: 0, height: '60vh' }}
        />
      )}
    </div>
  )
}

// Render Virtuoso as a semantic <ul>/<li> tree so screen readers see
// "list, N items, item 1 of N". We merge our cosmetic overrides AFTER
// Virtuoso's positional ones (paddingTop/paddingBottom on the list,
// height on each item) so the scroller spacer math still works.
const VirtuosoList = forwardRef<HTMLUListElement, React.HTMLAttributes<HTMLUListElement>>(
  function VirtuosoList(props, ref) {
    return (
      <ul
        {...props}
        ref={ref}
        style={{
          ...(props.style ?? {}),
          listStyle: 'none',
          margin: 0,
        }}
      />
    )
  },
)
const VirtuosoItem = forwardRef<HTMLLIElement, React.LiHTMLAttributes<HTMLLIElement>>(
  function VirtuosoItem(props, ref) {
    return (
      <li
        {...props}
        ref={ref}
        style={{ ...(props.style ?? {}), paddingBottom: 8 }}
      />
    )
  },
)

const virtuosoListComponents = {
  List: VirtuosoList,
  Item: VirtuosoItem,
}

interface DecisionCardProps {
  record: DecisionsResponse['decisions'][number]
  onJumpToTask?: (taskId: string) => void
  dag?: DAG | null
}

function DecisionCard({ record, onJumpToTask, dag }: DecisionCardProps): JSX.Element {
  const _kind = record.decision.kind
  const stage = (record.decision['stage'] ?? record.decision['target_stage']) as
    | string
    | undefined
  const taskId = record.decision['task_id'] as string | undefined
  // Resolve stage_class → task id by scanning the DAG. Without this,
  // "Open step" deep-links to a hash that doesn't match any task and
  // silently does nothing. Falls back to null when no task matches the
  // stage; the button is hidden in that case.
  const linkTarget = useMemo(() => {
    if (taskId) return taskId
    if (!stage || !dag) return null
    for (const [id, task] of Object.entries(dag.tasks)) {
      if (!task) continue
      const sc = (task.spec as Record<string, unknown> | null)?.['stage_class']
      if (sc === stage) return id
    }
    return null
  }, [stage, taskId, dag])
  const actorLabel = record.actor === 'sme' ? 'You' : record.actor === 'llm' ? 'The agent' : 'The system'

  return (
    // Rendered inside a Virtuoso <li> wrapper, so this is a <div> — a
    // nested <li> would invalidate the semantic structure and confuse
    // screen-reader navigation.
    <div
      style={{
        background: 'var(--color-surface-1)',
        border: '1px solid var(--color-border-default)',
        borderRadius: 8,
        padding: '10px 12px',
      }}
    >
      <div
        style={{ fontSize: '0.72rem', color: 'var(--color-text-muted)' }}
        title={new Date(record.timestamp).toLocaleString()}
      >
        {relativeTime(record.timestamp)} • {actorLabel.toLowerCase()}
      </div>
      <div style={{ marginTop: 2, fontSize: '0.92rem', color: 'var(--color-text-primary)' }}>
        <strong>{actorLabel}</strong>{' '}
        {headline(record.decision as unknown as DecisionType)}
      </div>
      {record.rationale && (
        <div
          style={{
            marginTop: 4,
            fontSize: '0.82rem',
            color: 'var(--color-text-secondary)',
            fontStyle: 'italic',
            background: 'var(--color-surface-0)',
            borderRadius: 4,
            padding: '6px 8px',
          }}
        >
          “{record.rationale}”
          {record.rationale.length > 80 && (
            <ExplainButton text={record.rationale} context="decision rationale" />
          )}
        </div>
      )}
      {linkTarget && onJumpToTask && (
        <button
          type="button"
          onClick={() => onJumpToTask(linkTarget)}
          style={{
            marginTop: 8,
            padding: '3px 9px',
            fontSize: '0.75rem',
            borderRadius: 4,
            border: '1px solid var(--color-border-strong)',
            background: 'var(--color-surface-1)',
            color: 'var(--color-text-secondary)',
            cursor: 'pointer',
          }}
        >
          Open step
        </button>
      )}
    </div>
  )
}

function headline(decision: DecisionType): string {
  switch (decision.kind) {
    case 'confirm':
      return 'confirmed the plan'
    case 'reject':
      return 'rejected the plan and asked for changes'
    case 'unblock':
      return 'cleared a blocker'
    case 'branch':
      return 'created a branched session to explore an alternative'
    case 'emit_package':
      return 'emitted the package'
    case 'amend_stage':
      return `changed the method for ${decision.stage}`
    case 'rerun_task':
      return `reran ${decision.task_id}`
    case 'post_hoc_deviation':
      return `deviated from the pre-specified method on ${decision.target_stage}`
    case 'select_sensitivity_winner':
      return `picked a sensitivity-comparison winner on ${decision.stage}`
    case 'cross_version_diff':
      return 'recorded a cross-version concordance report'
    case 'auto_advanced':
      return `auto-advanced ${decision.stage}`
    case 'undone_amendment':
      return `undid a method change on ${decision.stage}`
    case 'budget_changed':
      return 'updated the session budget'
    case 'user_note':
      return `added a note on ${decision.task_id}`
    case 'applied_structured_decision':
      return `applied a structured decision on ${decision.task_id}`
    case 'disposition_proposed':
      return `proposed ${decision.action_count} change${decision.action_count === 1 ? '' : 's'} on ${decision.task_id}`
    case 'disposition_applied':
      return `applied a disposition on ${decision.target_stage} (${decision.outcome})`
    case 'disposition_rejected':
      return 'rejected an agent-proposed disposition'
    case 'set_intake_field':
      return `set ${decision.field} on ${decision.stage}`
    case 'set_intake_method':
      return `recorded the method for ${decision.stage}`
    case 'append_intake_prose':
      return decision.modality_changed
        ? `appended intake prose; modality reclassified to ${decision.classified_modality}`
        : 'appended intake prose'
    case 'assumption_recorded':
      return `recorded assumption "${decision.statement}" (risk: ${decision.risk})`
    case 'assumption_resolved':
      return `resolved assumption ${decision.id} (${decision.resolution})`
    case 'proposed_hypothesized_node':
      return `proposed hypothesized node ${decision.node_id}`
    case 'plot_affordance_resolved':
      return `resolved plot affordance for ${decision.task_id}/${decision.port_name} → ${decision.affordance_variant}`
    case 'plot_affordance_fallback':
      return `structural fallback affordance for ${decision.task_id}/${decision.port_name} (${decision.primitive})`
    case 'proposed_hypothesized_renderer':
      return `proposed a custom renderer for ${decision.target_semantic_type}`
    case 'renderer_draft_requested':
      return `requested renderer draft for proposal ${decision.proposal_id}`
    case 'renderer_draft_received':
      return `received renderer draft for proposal ${decision.proposal_id}`
    case 'renderer_sandbox_outcome':
      return `sandbox check for proposal ${decision.proposal_id}: ${decision.outcome}`
    case 'approve_generated_renderer':
      return `approved generated renderer for proposal ${decision.proposal_id}`
    case 'reject_generated_renderer':
      return `rejected generated renderer for proposal ${decision.proposal_id}`
    case 'promoted_generated_renderer':
      return `promoted renderer for proposal ${decision.proposal_id} → ${decision.target_stage_id}`
    case 'adapter_decision_recorded':
      return `${decision.decision} adapter ${decision.adapter_id} (${decision.safety})`
    case 'novel_node_decision_recorded':
      return `${decision.decision} novel node ${decision.node_id}`
    case 'refusal_acknowledged':
      return `acknowledged refusal ${decision.refusal_id} via ${decision.recovery}`
    case 'assumption_contradicted':
      return `contradicted assumption ${decision.assumption_id} (prior ${decision.prior_confirmation_id} vs ${decision.conflicting_confirmation_id})`
    case 'assumption_waived':
      return `waived assumption ${decision.assumption_id} under policy ${decision.policy_rule_id}`
    case 'assumption_invalidated':
      return `invalidated assumption ${decision.assumption_id} due to ${decision.upstream_change}`
    case 'lifecycle_transition':
      return `recorded a lifecycle ${decision.transition_kind} transition`
    case 'contradiction_detected':
      return `detected contradiction on assumption ${decision.assumption_id} (prior ${decision.prior_id} vs new ${decision.new_id})`
    case 'invalidation_cascaded':
      return `cascaded invalidation of assumption ${decision.assumption_id} to ${decision.affected.length} downstream entr${decision.affected.length === 1 ? 'y' : 'ies'}`
    case 'proposal_rejected': {
      const rationale = decision.rationale
        ? ` — ${decision.rationale}`
        : ''
      return `rejected hypothesized proposal ${decision.proposal_id}${rationale}`
    }
    case 'runtime_package_added':
      return `widened ${decision.atom_id}.runtime_packages with ${decision.package} from ${decision.registry}`
    case 'claim_verification':
      return `verified ${decision.n_verified} claim${decision.n_verified === 1 ? '' : 's'} against ${decision.task_id} (${decision.n_mismatch} mismatch${decision.n_mismatch === 1 ? '' : 'es'})`
  }
  // Exhaustiveness: tsc fails when a new DecisionType variant is added
  // upstream and lands in ui/src/types/DecisionType.ts via `make types`.
  const _exhaustive: never = decision
  void _exhaustive
  return 'recorded a decision'
}

/**
 * Surface the install-proxy events recorded
 * in `runtime/install-log.jsonl`. Each row carries `atom_id`,
 * `package`, `registry`, and `timestamp`. Renders nothing when the
 * list is empty so the tab stays tidy on sessions that didn't trigger
 * any runtime installs (the dominant case for Sealed / DeclaredOnly
 * packages with everything vendored).
 */
function InstallLogSection({
  entries,
}: {
  entries: Array<Record<string, unknown>>
}): JSX.Element | null {
  if (entries.length === 0) return null
  return (
    <section
      aria-label="Runtime install log"
      style={{
        background: 'var(--color-surface-1)',
        border: '1px solid var(--color-border-default)',
        borderRadius: 8,
        padding: '10px 12px',
      }}
    >
      <h3
        style={{
          margin: 0,
          fontSize: '0.78rem',
          textTransform: 'uppercase',
          letterSpacing: '0.04em',
          color: 'var(--color-text-secondary)',
        }}
      >
        Runtime install log
      </h3>
      <p
        style={{
          margin: '4px 0 8px',
          fontSize: '0.78rem',
          color: 'var(--color-text-muted)',
        }}
      >
        Packages the agent installed at task time via the install
        proxy. Sealed atoms never appear here.
      </p>
      <ul
        data-testid="install-log-list"
        style={{
          listStyle: 'none',
          margin: 0,
          padding: 0,
          display: 'flex',
          flexDirection: 'column',
          gap: 4,
        }}
      >
        {entries.map((row, i) => {
          const atomId = String(row['atom_id'] ?? 'unknown')
          const pkg = String(row['package'] ?? '?')
          const registry = String(row['registry'] ?? '?')
          const ts = row['timestamp']
          // The proxy serialises `timestamp` as an epoch float (seconds).
          // We render it as a localised string when possible; otherwise
          // we surface the raw value so the SME isn't left guessing.
          const tsLabel = (() => {
            if (typeof ts === 'number' && Number.isFinite(ts)) {
              try {
                return new Date(ts * 1000).toLocaleString()
              } catch {
                return String(ts)
              }
            }
            if (typeof ts === 'string') return ts
            return ''
          })()
          return (
            <li
              key={i}
              data-testid="install-log-row"
              style={{
                display: 'grid',
                gridTemplateColumns: '1fr 1fr 80px 1fr',
                gap: 8,
                fontSize: '0.78rem',
                padding: '3px 0',
                borderBottom: '1px solid var(--color-border-default)',
                color: 'var(--color-text-primary)',
              }}
            >
              <span
                title={atomId}
                style={{
                  overflow: 'hidden',
                  textOverflow: 'ellipsis',
                  whiteSpace: 'nowrap',
                }}
              >
                {atomId}
              </span>
              <span
                title={pkg}
                style={{
                  overflow: 'hidden',
                  textOverflow: 'ellipsis',
                  whiteSpace: 'nowrap',
                }}
              >
                {pkg}
              </span>
              <span style={{ color: 'var(--color-text-secondary)' }}>
                {registry}
              </span>
              <span
                style={{
                  color: 'var(--color-text-muted)',
                  overflow: 'hidden',
                  textOverflow: 'ellipsis',
                  whiteSpace: 'nowrap',
                }}
                title={tsLabel}
              >
                {tsLabel}
              </span>
            </li>
          )
        })}
      </ul>
    </section>
  )
}
