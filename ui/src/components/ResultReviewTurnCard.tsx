// Inline turn-card rendering for the `get_task_result` tool output.
// Renders the structured fields from `crates/conversation::tools::
// get_task_result`: task_id, status, description, kind, and the
// task-shape-specific body (result | reason | record). Carries an
// inline "Rerun" button that expands a reason textarea before
// dispatching onRerun(taskId, reason?) into the rerun_task tool.

import { useState } from 'react'
import { artifactUrl } from '../api/chatClient'
import { useCancelableEffect } from '../hooks/useCancelableFetch'
import type {
  ClaimVerificationReport,
  CrossVersionReport,
  TaskKind,
} from '../types'
import type { PlotAffordance } from '../types/PlotAffordance'
import CrossVersionDiffCard from './CrossVersionDiffCard'
import IterationConvergenceChart, {
  parseIterateResult,
} from './IterationConvergenceChart'
import { useAsync } from '../hooks/useAsync'
import { CardContainer } from './primitives/CardContainer'
import { OptionalTextarea } from './primitives/OptionalTextarea'
import { SubmitCancelRow } from './primitives/SubmitCancelRow'
import { CARD_PALETTES, type CardPalette } from '../styles/palettes'
import { stageIdToLabel } from '../lib/stageLabels'
import { sanitizeForSme } from '../lib/smeText'
import { AffordanceBadge } from './AffordanceBadge'
import { RendererProposalCard } from './RendererProposalCard'

export type ResultStatus = 'completed' | 'failed' | 'blocked'

export interface ResultReviewPayload {
  task_id: string
  status: ResultStatus
  description: string
  kind: TaskKind
  /** Present when status === 'completed'. Typed unknown because the
   *  agent writes whatever task-specific JSON shape it produces. */
  result?: unknown
  /** Present when status === 'failed'. */
  reason?: string
  /** Present when status === 'blocked'. */
  record?: unknown
  /**
   * Present when `get_task_result` ran the narrative claim verifier
   * over this task's output. Absent means either no narrative artifact
   * was present, no `verifiableEntities` policy was configured, or the
   * task is still running. The card renders a compact summary badge
   * and expandable verdict list when present.
   */
  verification?: ClaimVerificationReport | null
  /**
   * Cross-version concordance report for the session, attached to the
   * task result when a branch/amendment re-emit has produced a diff.
   * Drives the inline "Cross-version" summary row + "Open diff" button
   * that launches `CrossVersionDiffCard`.
   */
  cross_version_diff?: CrossVersionReport | null
}

interface Props {
  payload: ResultReviewPayload
  /**
   * When provided, the card shows a Rerun button. Clicking it expands
   * an inline reason textarea; submitting dispatches `onRerun(taskId,
   * reason?)` so the caller can annotate *why* they're requesting a
   * rerun. Wired to the rerun_task tool in the conversation pane.
   */
  onRerun?: (taskId: string, reason?: string) => void | Promise<void>
  /**
   * Session id threaded through so the inline `CrossVersionDiffCard`
   * can call the `/cross-version-diff` endpoint to fetch the full
   * report when the user clicks Open diff.
   */
  sessionId?: string | null
  /**
   * When the session is in confirmatory mode, render a "Confirmatory"
   * badge alongside the verification summary so reviewers see the
   * discipline at a glance. Defaults to false (Exploratory).
   */
  isConfirmatory?: boolean
  /**
   * Affordance map keyed by figure id. When present, the
   * FigureStrip renders an `AffordanceBadge` per figure and, for
   * non-Registered variants, a "Describe a preferred plot" CTA that
   * opens the `RendererProposalCard` inline.
   *
   * The actual fetch from the results-review API endpoint is a separate
   * concern. The parent component may thread a snapshot read from
   * `runtime/affordance_fallbacks.jsonl` or leave this prop absent
   * (silently renders without badges).
   */
  affordances?: Record<string, PlotAffordance> | null
}

function CrossVersionSummary({
  report,
  onOpen,
}: {
  report: CrossVersionReport
  onOpen: () => void
}): JSX.Element {
  const totalOverlap = report.tables.reduce((acc, t) => acc + t.n_overlap, 0)
  const totalDiscordant = report.tables.reduce((acc, t) => acc + t.n_discordant, 0)
  const totalRobust = report.tables.reduce((acc, t) => acc + t.n_robust, 0)
  const totalConcordant = report.tables.reduce((acc, t) => acc + t.n_concordant, 0)
  const tone = totalDiscordant > 0 ? 'discordant' : 'concordant'
  const paletteKey: CardPalette = tone === 'discordant' ? 'danger' : 'success'
  const tonePalette = CARD_PALETTES[paletteKey]
  return (
    <CardContainer
      palette={paletteKey}
      ariaLabel="Cross-version diff summary"
      dataAttrs={{ 'data-cross-version-tone': tone }}
      style={{
        marginTop: '0.6rem',
        padding: '0.55rem 0.75rem',
        fontSize: '0.78rem',
        color: tonePalette.fg,
        display: 'flex',
        justifyContent: 'space-between',
        alignItems: 'baseline',
        gap: '0.5rem',
        borderLeft: `1px solid ${tonePalette.border}`,
      }}
    >
      <strong>
        Cross-version ({totalOverlap} rows): {totalRobust} robust · {totalConcordant}{' '}
        concordant · {totalDiscordant} discordant
      </strong>
      <button
        type="button"
        onClick={onOpen}
        style={{
          padding: '0.2rem 0.55rem',
          fontSize: '0.7rem',
          color: tonePalette.fg,
          background: 'transparent',
          border: `1px solid ${tonePalette.border}`,
          borderRadius: 4,
          cursor: 'pointer',
          fontWeight: 600,
        }}
      >
        Open diff
      </button>
    </CardContainer>
  )
}

// Status-driven palette map. Each status picks one of the shared
// palette tokens so the outer CardContainer and the rerun
// SubmitCancelRow stay in visual sync without duplicating hex literals.
const STATUS_PALETTES: Record<ResultStatus, CardPalette> = {
  completed: 'success',
  failed: 'danger',
  blocked: 'warning',
}

function renderResultBody(payload: ResultReviewPayload): JSX.Element {
  if (payload.status === 'completed') {
    // When the agent wrote an iterate-until check result
    // (metric_trail array), surface the convergence trajectory chart
    // above the JSON dump so the SME sees the loop's behavior at a
    // glance rather than digging through the raw envelope. The chart
    // is purely additive — the JSON pre still renders.
    const iterateParsed = parseIterateResult(payload.result)
    const resultPretty = JSON.stringify(payload.result ?? {}, null, 2)
    return (
      <>
        {iterateParsed && (
          <IterationConvergenceChart
            trail={iterateParsed.trail}
            threshold={iterateParsed.threshold}
            operator={iterateParsed.operator}
            convergedAtIter={iterateParsed.convergedAtIter}
          />
        )}
        <pre
          aria-label="Task result JSON"
          style={{
            marginTop: '0.5rem',
            padding: '0.65rem 0.8rem',
            background: 'var(--color-surface-0)',
            border: '1px solid #e2e8f0',
            borderRadius: 6,
            fontFamily:
              'ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace',
            fontSize: '0.74rem',
            lineHeight: 1.45,
            color: 'var(--color-text-secondary)',
            maxHeight: 320,
            overflow: 'auto',
            whiteSpace: 'pre-wrap',
            wordBreak: 'break-word',
          }}
        >
          {resultPretty}
        </pre>
      </>
    )
  }
  if (payload.status === 'failed') {
    return (
      <p
        style={{
          marginTop: '0.5rem',
          fontSize: '0.83rem',
          color: 'var(--color-warning-fg)',
        }}
      >
        <strong>Reason: </strong>
        {payload.reason ? sanitizeForSme(payload.reason) : 'The agent did not provide details.'}
      </p>
    )
  }
  const blockedPretty = JSON.stringify(payload.record ?? {}, null, 2)
  return (
    <pre
      aria-label="Blocked record"
      style={{
        marginTop: '0.5rem',
        padding: '0.65rem 0.8rem',
        background: 'var(--color-warning-bg)',
        border: '1px solid #fcd34d',
        borderRadius: 6,
        fontFamily:
          'ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace',
        fontSize: '0.74rem',
        color: 'var(--color-warning-fg)',
        maxHeight: 240,
        overflow: 'auto',
        whiteSpace: 'pre-wrap',
        wordBreak: 'break-word',
      }}
    >
      {blockedPretty}
    </pre>
  )
}

function VerificationPanel({
  report,
  isConfirmatory = false,
}: {
  report: ClaimVerificationReport
  isConfirmatory?: boolean
}): JSX.Element | null {
  const [expanded, setExpanded] = useState(false)
  const [postHocOpen, setPostHocOpen] = useState(false)
  if (report.n_checked === 0) {
    return (
      <p
        aria-label="Claim verification summary"
        data-verification-empty="true"
        style={{
          marginTop: '0.55rem',
          fontSize: '0.74rem',
          color: 'var(--color-text-secondary)',
          fontStyle: 'italic',
        }}
      >
        No verifiable claims found in this task's narrative.
      </p>
    )
  }
  const tone = report.n_mismatch > 0 ? 'mismatch' : report.n_unverifiable > 0 ? 'partial' : 'clean'
  const paletteKey: CardPalette =
    tone === 'mismatch' ? 'danger' : tone === 'partial' ? 'warning' : 'success'
  const tonePalette = CARD_PALETTES[paletteKey]
  return (
    <CardContainer
      palette={paletteKey}
      ariaLabel="Claim verification summary"
      dataAttrs={{ 'data-verification-tone': tone }}
      style={{
        marginTop: '0.6rem',
        padding: '0.55rem 0.75rem',
        fontSize: '0.78rem',
        color: tonePalette.fg,
        borderLeft: `1px solid ${tonePalette.border}`,
      }}
    >
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'baseline' }}>
        <strong style={{ fontSize: '0.8rem' }}>
          {isConfirmatory && (
            // A separate "Confirmatory" discipline badge so reviewers
            // see at a glance whether the session is operating under
            // SAP-style discipline.
            <span
              aria-label="confirmatory discipline"
              style={{
                marginRight: '0.45rem',
                padding: '0.05rem 0.45rem',
                background: 'var(--color-info-bg)',
                color: 'var(--color-info-fg)',
                border: '1px solid #a5b4fc',
                borderRadius: 999,
                fontSize: '0.68rem',
                fontWeight: 600,
                letterSpacing: '0.02em',
              }}
            >
              Confirmatory
            </span>
          )}
          Claim verification — {report.n_verified}/{report.n_checked} verified
          {report.n_mismatch > 0 ? `, ${report.n_mismatch} mismatch${report.n_mismatch === 1 ? '' : 'es'}` : ''}
          {report.n_unverifiable > 0 ? `, ${report.n_unverifiable} unverifiable` : ''}
          {(() => {
            // The post-hoc pill is a button that opens a drill-down
            // list of every claim demoted to `strength: post_hoc`.
            const postHoc = report.verdicts.filter((v) => v.strength === 'post_hoc').length
            return postHoc > 0 ? (
              <button
                type="button"
                aria-label="post-hoc claim deviations"
                aria-expanded={postHocOpen}
                onClick={() => setPostHocOpen((open) => !open)}
                style={{
                  marginLeft: '0.45rem',
                  padding: '0.05rem 0.45rem',
                  background: 'var(--color-warning-bg)',
                  color: 'var(--color-warning-fg)',
                  border: '1px solid #fbbf24',
                  borderRadius: 999,
                  fontSize: '0.68rem',
                  fontWeight: 600,
                  letterSpacing: '0.02em',
                  cursor: 'pointer',
                }}
              >
                ⚠ {postHoc} post-hoc
              </button>
            ) : null
          })()}
          {report.runtime_decision_log_path ? (
            <a
              href={`runtime/${report.runtime_decision_log_path.replace(/^runtime\//, '')}`}
              style={{
                marginLeft: '0.45rem',
                fontSize: '0.68rem',
                color: tonePalette.fg,
                textDecoration: 'underline',
              }}
              aria-label="agent-runtime decision log"
            >
              runtime log
            </a>
          ) : null}
        </strong>
        <button
          type="button"
          onClick={() => setExpanded((e) => !e)}
          aria-expanded={expanded}
          style={{
            padding: '0.2rem 0.55rem',
            fontSize: '0.7rem',
            color: tonePalette.fg,
            background: 'transparent',
            border: `1px solid ${tonePalette.border}`,
            borderRadius: 4,
            cursor: 'pointer',
            fontWeight: 600,
          }}
        >
          {expanded ? 'Hide detail' : 'Show detail'}
        </button>
      </div>
      {expanded && (
        <ul
          data-verification-detail="open"
          style={{
            listStyle: 'none',
            padding: 0,
            margin: '0.55rem 0 0',
            display: 'flex',
            flexDirection: 'column',
            gap: '0.4rem',
          }}
        >
          {report.verdicts.map((v, idx) => {
            const statusLabel = v.status.status
            const icon = statusLabel === 'verified' ? '✓' : statusLabel === 'mismatch' ? '✗' : '?'
            const detail =
              statusLabel === 'mismatch'
                ? v.status.detail
                : statusLabel === 'unverifiable'
                  ? v.status.reason
                  : null
            // 6-color category chip per verdict.
            // Cross of {verified, mismatch, unverifiable} × {prespecified,
            // post_hoc, exploratory} → 6 distinct categories that map to
            // the established 6-color palette used by CrossVersionDiffCard:
            //
            // Robust ← verified + prespecified (success)
            // Concordant ← verified + exploratory (warning)
            // Discordant ← mismatch + any (danger)
            // New_in_child ← verified + post_hoc (accent)
            // Dropped_in_parent ← mismatch + post_hoc (muted)
            // Unverifiable ← unverifiable + any (faint)
            const chipColor =
              statusLabel === 'mismatch' && v.strength === 'post_hoc'
                ? 'var(--color-text-muted)'
                : statusLabel === 'mismatch'
                  ? 'var(--color-danger-accent)'
                  : statusLabel === 'unverifiable'
                    ? 'var(--color-text-faint)'
                    : v.strength === 'prespecified'
                      ? 'var(--color-success-accent)'
                      : v.strength === 'post_hoc'
                        ? 'var(--color-accent)'
                        : 'var(--color-warning-accent)'
            return (
              <li
                key={`${v.claim.entity}-${idx}`}
                data-verdict-status={statusLabel}
                data-verdict-strength={v.strength}
                style={{
                  padding: '0.4rem 0.5rem',
                  background: 'var(--color-surface-1)',
                  border: `1px solid ${tonePalette.border}`,
                  borderRadius: 4,
                  fontSize: '0.75rem',
                  lineHeight: 1.4,
                }}
              >
                <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
                  <span
                    aria-hidden="true"
                    title={`${statusLabel} / ${v.strength}`}
                    style={{
                      display: 'inline-block',
                      width: 8,
                      height: 8,
                      borderRadius: '50%',
                      background: chipColor,
                      flexShrink: 0,
                    }}
                  />
                  <span aria-hidden="true" style={{ fontWeight: 700 }}>
                    {icon}
                  </span>
                  <strong>{v.claim.entity}</strong>
                  {v.claim.direction ? ` — ${v.claim.direction}` : ''}
                  {v.claim.effect_size !== null && v.claim.effect_size !== undefined
                    ? `, effect=${v.claim.effect_size}`
                    : ''}
                  {v.claim.source_table ? ` (${v.claim.source_table})` : ''}
                </div>
                {detail && (
                  <div style={{ marginTop: 4, color: 'var(--color-text-secondary)', fontStyle: 'italic' }}>{detail}</div>
                )}
              </li>
            )
          })}
        </ul>
      )}
      {postHocOpen && (
        <ul
          data-post-hoc-detail="open"
          aria-label="post-hoc deviated claims"
          style={{
            listStyle: 'none',
            padding: 0,
            margin: '0.55rem 0 0',
            display: 'flex',
            flexDirection: 'column',
            gap: '0.4rem',
          }}
        >
          {report.verdicts
            .filter((v) => v.strength === 'post_hoc')
            .map((v, idx) => (
              <li
                key={`posthoc-${v.claim.entity}-${idx}`}
                style={{
                  padding: '0.4rem 0.5rem',
                  background: 'var(--color-warning-bg)',
                  border: '1px solid #fbbf24',
                  borderRadius: 4,
                  fontSize: '0.75rem',
                  lineHeight: 1.4,
                  color: 'var(--color-warning-fg)',
                }}
              >
                <strong>{v.claim.entity}</strong> — demoted (post-hoc).
                {v.claim.source_table ? (
                  <> Cited table: <code>{v.claim.source_table}</code>.</>
                ) : null}
                <div style={{ marginTop: '0.2rem', fontStyle: 'italic' }}>
                  {v.claim.excerpt}
                </div>
              </li>
            ))}
        </ul>
      )}
    </CardContainer>
  )
}

interface FiguresManifestShape {
  stage_id: string
  written: Record<string, string>
  skipped: Record<string, string>
  errors: Record<string, string>
}

/**
 * Horizontal figure strip shown below the result body. Fetches
 * `runtime/outputs/<task_id>/figures/manifest.json` on mount; silent
 * when absent (task's stage didn't declare required_figures, or figures
 * haven't been written yet). Clicking a thumbnail opens it in a new
 * tab at full resolution — lightweight by design, since the
 * `TaskLogDrawer` Figures tab already carries the lightbox.
 *
 * When `affordances` is provided, renders an `AffordanceBadge`
 * per figure and, for non-Registered variants, a "Describe a preferred plot"
 * button that opens the `RendererProposalCard` inline.
 */
function FigureStrip({
  sessionId,
  taskId,
  affordances,
}: {
  sessionId: string
  taskId: string
  /**
   * Keyed by figure id. When absent (or a key is missing) the strip
   * renders without a badge for that figure — graceful degradation so
   * callers that don't yet have affordance data still work.
   */
  affordances?: Record<string, PlotAffordance> | null
}): JSX.Element | null {
  const [manifest, setManifest] = useState<FiguresManifestShape | null>(null)
  const [hadError, setHadError] = useState(false)
  // figureId whose RendererProposalCard is currently open (null = none).
  const [proposalOpenFor, setProposalOpenFor] = useState<string | null>(null)
  const [smeConfirmed, setSmeConfirmed] = useState<Set<string>>(new Set())

  useCancelableEffect(async ({ signal, cancelled }) => {
    // discover_* and validate_* never write figures; skip the fetch.
    if (taskId.startsWith('discover_') || taskId.startsWith('validate_')) {
      return
    }
    const url = artifactUrl(
      sessionId,
      `runtime/outputs/${encodeURIComponent(taskId)}/figures/manifest.json`,
    )
    try {
      const r = await fetch(url, { signal }) // allow-bare-fetch: artifactUrl carries share-token
      if (!r.ok) {
        if (!cancelled()) setHadError(true)
        return
      }
      const data = (await r.json()) as FiguresManifestShape
      if (!cancelled()) setManifest(data)
    } catch {
      if (!cancelled()) setHadError(true)
    }
  }, [sessionId, taskId])

  if (hadError || !manifest) return null
  const written = manifest.written ?? {}
  const entries = Object.entries(written)
  if (entries.length === 0) return null

  return (
    <section
      aria-label="Task figures"
      data-figure-strip="true"
      style={{
        marginTop: '0.6rem',
        padding: '0.45rem 0.55rem',
        background: 'var(--color-surface-0)',
        border: '1px solid #e2e8f0',
        borderRadius: 6,
      }}
    >
      <div
        style={{
          fontSize: '0.7rem',
          color: 'var(--color-text-secondary)',
          marginBottom: 4,
          fontWeight: 600,
          textTransform: 'uppercase',
          letterSpacing: '0.04em',
        }}
      >
        Figures ({entries.length})
      </div>
      <div
        style={{
          display: 'flex',
          gap: '0.45rem',
          overflowX: 'auto',
        }}
      >
        {entries.map(([id, absPath]) => {
          const url = deriveFigureUrl(sessionId, taskId, id, absPath)
          const affordance = affordances?.[id] ?? null
          const isRegistered = affordance?.kind === 'registered'
          const isConfirmed = smeConfirmed.has(id)

          return (
            <div
              key={id}
              style={{
                flexShrink: 0,
                display: 'flex',
                flexDirection: 'column',
                gap: '0.25rem',
                alignItems: 'flex-start',
              }}
            >
              <a
                href={url}
                target="_blank"
                rel="noreferrer"
                title={id}
                data-figure-id={id}
                style={{
                  width: 120,
                  height: 90,
                  background: 'var(--color-surface-1)',
                  border: '1px solid #cbd5e1',
                  borderRadius: 4,
                  overflow: 'hidden',
                  display: 'block',
                }}
              >
                <img
                  src={url}
                  alt={id}
                  loading="lazy"
                  style={{
                    width: '100%',
                    height: '100%',
                    objectFit: 'contain',
                    display: 'block',
                  }}
                />
              </a>
              {affordance && (
                <AffordanceBadge affordance={affordance} />
              )}
              {affordance && !isRegistered && !isConfirmed && (
                <div style={{ display: 'flex', gap: '0.25rem', flexWrap: 'wrap' }}>
                  <button
                    type="button"
                    onClick={() => setSmeConfirmed((s) => new Set(s).add(id))}
                    aria-label={`Confirm figure ${id} as acceptable`}
                    style={{
                      padding: '1px 7px',
                      fontSize: '0.7rem',
                      background: 'transparent',
                      color: 'var(--color-text-secondary)',
                      border: '1px solid var(--color-border-default)',
                      borderRadius: 3,
                      cursor: 'pointer',
                    }}
                  >
                    OK
                  </button>
                  <button
                    type="button"
                    onClick={() => setProposalOpenFor(id)}
                    aria-label={`Describe a preferred plot for figure ${id}`}
                    style={{
                      padding: '1px 7px',
                      fontSize: '0.7rem',
                      background: 'transparent',
                      color: 'var(--color-info-fg)',
                      border: '1px solid var(--color-info-border)',
                      borderRadius: 3,
                      cursor: 'pointer',
                      whiteSpace: 'nowrap',
                    }}
                  >
                    Describe a plot
                  </button>
                </div>
              )}
              {affordance && !isRegistered && isConfirmed && (
                <span
                  style={{
                    fontSize: '0.68rem',
                    color: 'var(--color-text-muted)',
                    fontStyle: 'italic',
                  }}
                >
                  Confirmed
                </span>
              )}
              {proposalOpenFor === id && affordance != null && (
                <RendererProposalCard
                  sessionId={sessionId}
                  targetSemanticType={affordance.proof.source_semantic_type}
                  primitiveBasis={
                    affordance.kind === 'structural_fallback' ? affordance.primitive : null
                  }
                  availableParentTerms={affordance.proof.ontology_walk}
                  onAccepted={(_proposalId) => {
                    setProposalOpenFor(null)
                    setSmeConfirmed((s) => new Set(s).add(id))
                  }}
                  onCancel={() => setProposalOpenFor(null)}
                />
              )}
            </div>
          )
        })}
      </div>
    </section>
  )
}

function deriveFigureUrl(
  sessionId: string,
  taskId: string,
  figureId: string,
  absPath: string,
): string {
  const marker = '/runtime/'
  const i = absPath.indexOf(marker)
  if (i >= 0) {
    const relative = absPath.slice(i + 1)
    return artifactUrl(sessionId, relative)
  }
  return artifactUrl(
    sessionId,
    `runtime/outputs/${encodeURIComponent(taskId)}/figures/${encodeURIComponent(figureId)}.png`,
  )
}

export default function ResultReviewTurnCard({
  payload,
  onRerun,
  sessionId,
  isConfirmatory = false,
  affordances,
}: Props) {
  const paletteKey = STATUS_PALETTES[payload.status]
  const pal = CARD_PALETTES[paletteKey]
  const [rerunExpanded, setRerunExpanded] = useState(false)
  const [reason, setReason] = useState('')
  const { busy: submitting, run } = useAsync()
  const [diffOpen, setDiffOpen] = useState(false)

  const handleRerunClick = () => {
    setRerunExpanded(true)
  }
  const handleRerunCancel = () => {
    setRerunExpanded(false)
    setReason('')
  }
  const handleRerunSubmit = async () => {
    if (!onRerun) return
    const trimmed = reason.trim()
    await run(() =>
      Promise.resolve(onRerun(payload.task_id, trimmed ? trimmed : undefined)),
    )
    setRerunExpanded(false)
    setReason('')
  }

  return (
    <CardContainer
      palette={paletteKey}
      role="region"
      ariaLabel={`Result for task ${payload.task_id}`}
      dataAttrs={{
        'data-result-status': payload.status,
        'data-task-id': payload.task_id,
      }}
    >
      <header
        style={{
          display: 'flex',
          alignItems: 'baseline',
          justifyContent: 'space-between',
          gap: '0.5rem',
          marginBottom: 6,
        }}
      >
        <strong
          style={{
            fontSize: '0.85rem',
            color: pal.fg,
          }}
          title={payload.task_id}
        >
          {stageIdToLabel(payload.task_id)}
        </strong>
        <span
          style={{
            fontSize: '0.72rem',
            textTransform: 'uppercase',
            letterSpacing: '0.04em',
            color: pal.fg,
            fontWeight: 600,
          }}
        >
          {payload.status}
        </span>
      </header>
      <p
        style={{
          margin: 0,
          fontSize: '0.81rem',
          color: pal.fg,
          lineHeight: 1.4,
        }}
      >
        {payload.description}
      </p>
      {renderResultBody(payload)}
      {sessionId && payload.status === 'completed' && (
        <FigureStrip
          sessionId={sessionId}
          taskId={payload.task_id}
          affordances={affordances}
        />
      )}
      {payload.verification && (
        <VerificationPanel report={payload.verification} isConfirmatory={isConfirmatory} />
      )}
      {payload.cross_version_diff && (
        <CrossVersionSummary
          report={payload.cross_version_diff}
          onOpen={() => setDiffOpen(true)}
        />
      )}
      {sessionId && payload.cross_version_diff && (
        <CrossVersionDiffCard
          sessionId={sessionId}
          open={diffOpen}
          onClose={() => setDiffOpen(false)}
          demotedEntities={
            payload.verification
              ? new Set(
                  payload.verification.verdicts
                    .filter((v) => v.strength === 'post_hoc')
                    .map((v) => v.claim.entity),
                )
              : undefined
          }
        />
      )}
      {onRerun && !rerunExpanded && (
        <div style={{ marginTop: '0.6rem' }}>
          <button
            type="button"
            onClick={handleRerunClick}
            style={{
              padding: '0.4rem 0.85rem',
              background: 'transparent',
              color: pal.fg,
              border: `1px solid ${pal.border}`,
              borderRadius: 6,
              cursor: 'pointer',
              fontSize: '0.78rem',
              fontWeight: 600,
            }}
          >
            Rerun this task
          </button>
        </div>
      )}
      {onRerun && rerunExpanded && (
        <div style={{ marginTop: '0.6rem' }} data-rerun-panel="open">
          <OptionalTextarea
            label="Optional reason for the rerun"
            ariaLabel="Optional rerun reason"
            placeholder="e.g. inputs changed upstream; refresh needed"
            value={reason}
            onChange={setReason}
            disabled={submitting}
            rows={2}
          />
          <SubmitCancelRow
            palette={paletteKey}
            submitLabel="Confirm rerun"
            cancelLabel="Cancel"
            onSubmit={handleRerunSubmit}
            onCancel={handleRerunCancel}
            busy={submitting}
          />
        </div>
      )}
    </CardContainer>
  )
}
