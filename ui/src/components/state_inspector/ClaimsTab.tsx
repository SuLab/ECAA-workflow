// Claim verification aggregate view across all completed tasks.
//
// Sources: the per-task `verification` field on
// `GET /api/chat/session/:id/task/:tid/result`. The server populates
// this when the package's interpretation policy declares a
// `verifiableEntities` block AND the task wrote a narrative artifact.
// Tasks without policies (e.g. discover_*, validate_*) return
// `verification: null` and render as "n/a" rather than a failure.
//
// One row per completed task. Click a row to expand and see the
// per-claim verdict list (entity, direction, effect size, p-value,
// status). Mismatches are highlighted; a "Re-verify" button on each
// row POSTs the verify endpoint and refreshes the row.

import { useCallback, useEffect, useMemo, useState } from 'react'
import type { DAG } from '../../types/DAG'
import type { ClaimVerificationReport } from '../../types/ClaimVerificationReport'
import type { ClaimVerdict } from '../../types/ClaimVerdict'
import type { ClaimStrength } from '../../types/ClaimStrength'
import { getTaskResult, verifyTask } from '../../api/chatClient'
import { sanitizeForSme } from '../../lib/smeText'

interface Props {
  sessionId: string | null
  dag: DAG | null
}

interface Row {
  taskId: string
  status:
    | 'loading'
    | 'no_policy'
    | 'pass'
    | 'mismatch'
    | 'unverified'
    | 'error'
  report: ClaimVerificationReport | null
  error?: string
  verifying?: boolean
}

const HEADING_STYLE: React.CSSProperties = {
  fontSize: 14,
  fontWeight: 600,
  marginBottom: 4,
}

const SUBHEAD_STYLE: React.CSSProperties = {
  color: 'var(--color-text-muted, #666)',
  fontSize: 12,
  marginBottom: 12,
}

const TABLE_STYLE: React.CSSProperties = {
  width: '100%',
  borderCollapse: 'collapse',
  fontSize: 13,
}

const TH_STYLE: React.CSSProperties = {
  textAlign: 'left',
  padding: '6px 8px',
  borderBottom: '1px solid var(--color-border-default, #e2e8f0)',
  fontWeight: 600,
  fontSize: 12,
  color: 'var(--color-text-muted, #666)',
}

const TD_STYLE: React.CSSProperties = {
  padding: '6px 8px',
  borderBottom: '1px solid var(--color-border-subtle, #f1f5f9)',
  verticalAlign: 'top',
}

function statusPillStyle(status: Row['status']): React.CSSProperties {
  const base: React.CSSProperties = {
    display: 'inline-block',
    padding: '1px 6px',
    borderRadius: 3,
    fontSize: 11,
    fontWeight: 600,
  }
  switch (status) {
    case 'pass':
      return { ...base, background: '#dcfce7', color: '#166534' }
    case 'mismatch':
      return { ...base, background: '#fef2f2', color: '#991b1b' }
    case 'unverified':
      // Amber: claims exist but none of them could be checked against a
      // table (typically: all unverifiable, OR the narrative had zero
      // claim sentences). Distinguished from green PASS so the SME does
      // not read "nothing was verified" as "everything checked out".
      return { ...base, background: '#fef3c7', color: '#92400e' }
    case 'no_policy':
      return { ...base, background: 'var(--color-surface-2, #f1f5f9)', color: 'var(--color-text-muted, #666)' }
    case 'error':
      return { ...base, background: '#fef3c7', color: '#92400e' }
    case 'loading':
      return { ...base, background: 'var(--color-surface-2, #f1f5f9)', color: 'var(--color-text-muted, #666)' }
  }
}

function labelFor(status: Row['status']): string {
  switch (status) {
    case 'pass':
      return 'PASS'
    case 'mismatch':
      return 'MISMATCH'
    case 'unverified':
      return 'UNVERIFIED'
    case 'no_policy':
      return 'n/a'
    case 'error':
      return 'error'
    case 'loading':
      return '…'
  }
}

function classifyReport(report: ClaimVerificationReport | null): Row['status'] {
  if (!report) return 'no_policy'
  if (report.n_mismatch > 0) return 'mismatch'
  if (report.n_verified > 0) return 'pass'
  // No mismatches AND no verified claims: either zero claims extracted
  // from the narrative or all claims were unverifiable. Either way
  // nothing was actually cross-checked — surface as amber rather than
  // collapsing to a green PASS.
  return 'unverified'
}

export function ClaimsTab({ sessionId, dag }: Props): JSX.Element {
  const [rows, setRows] = useState<Row[]>([])
  const [expanded, setExpanded] = useState<string | null>(null)

  const completedTaskIds = useMemo(() => {
    if (!dag) return [] as string[]
    return Object.entries(dag.tasks ?? {})
      .filter(([, task]) => {
        if (!task) return false
        const state = task.state as unknown
        if (state && typeof state === 'object' && 'result' in (state as object)) {
          return true
        }
        if (state && typeof state === 'object' && 'status' in (state as object)) {
          return (state as { status?: string }).status === 'completed'
        }
        return false
      })
      .map(([id]) => id)
  }, [dag])

  // Fetch verification for every completed task once per session +
  // task-set change. Refetches on demand via setRows in re-verify.
  useEffect(() => {
    if (!sessionId || completedTaskIds.length === 0) {
      setRows([])
      return
    }
    let cancelled = false
    setRows(completedTaskIds.map((id) => ({ taskId: id, status: 'loading', report: null })))
    void (async () => {
      const results: Row[] = []
      for (const taskId of completedTaskIds) {
        try {
          const r = await getTaskResult(sessionId, taskId)
          const v = r.verification ?? null
          results.push({ taskId, status: classifyReport(v), report: v })
        } catch (e) {
          results.push({
            taskId,
            status: 'error',
            report: null,
            error: e instanceof Error ? e.message : String(e),
          })
        }
        if (cancelled) return
      }
      if (!cancelled) setRows(results)
    })()
    return () => {
      cancelled = true
    }
  }, [sessionId, completedTaskIds])

  const reverify = useCallback(
    async (taskId: string) => {
      if (!sessionId) return
      setRows((prev) =>
        prev.map((r) => (r.taskId === taskId ? { ...r, verifying: true } : r)),
      )
      try {
        const res = await verifyTask(sessionId, taskId)
        const status = classifyReport(res.report)
        setRows((prev) =>
          prev.map((r) =>
            r.taskId === taskId
              ? { ...r, status, report: res.report, verifying: false }
              : r,
          ),
        )
      } catch (e) {
        setRows((prev) =>
          prev.map((r) =>
            r.taskId === taskId
              ? {
                  ...r,
                  status: 'error',
                  error: e instanceof Error ? e.message : String(e),
                  verifying: false,
                }
              : r,
          ),
        )
      }
    },
    [sessionId],
  )

  if (!sessionId) {
    return (
      <div style={{ padding: 16, color: 'var(--color-text-muted, #666)' }}>
        No session selected.
      </div>
    )
  }

  if (!dag) {
    return (
      <div style={{ padding: 16, color: 'var(--color-text-muted, #666)' }}>
        Waiting for the workflow to emit before checking claims.
      </div>
    )
  }

  if (completedTaskIds.length === 0) {
    return (
      <div style={{ padding: 16 }}>
        <div style={HEADING_STYLE}>Claim verification</div>
        <div style={SUBHEAD_STYLE}>
          No tasks have completed yet. Each completed task is cross-checked
          against the package's interpretation policy
          (<code>verifiableEntities</code>) when present.
        </div>
      </div>
    )
  }

  const totals = rows.reduce(
    (acc, r) => {
      if (r.status === 'pass') acc.pass += 1
      else if (r.status === 'mismatch') acc.mismatch += 1
      else if (r.status === 'unverified') acc.unverified += 1
      else if (r.status === 'no_policy') acc.no_policy += 1
      else if (r.status === 'error') acc.error += 1
      return acc
    },
    { pass: 0, mismatch: 0, unverified: 0, no_policy: 0, error: 0 },
  )

  return (
    <div style={{ padding: 16, overflowY: 'auto' }} data-testid="claims-tab">
      <div style={HEADING_STYLE}>Claim verification</div>
      <div style={SUBHEAD_STYLE}>
        Per-task cross-check of agent narrative against result tables. Only
        runs when the interpretation policy declares a{' '}
        <code>verifiableEntities</code> block. Pass / mismatch / unverified /{' '}
        <code>n/a</code> reflects per-task verification status; UNVERIFIED
        means claims were present but none could be checked.
      </div>
      <div style={{ marginBottom: 12, fontSize: 12 }} data-testid="claims-summary">
        <span style={statusPillStyle('pass')}>PASS {totals.pass}</span>{' '}
        <span style={statusPillStyle('mismatch')}>MISMATCH {totals.mismatch}</span>{' '}
        <span style={statusPillStyle('unverified')}>UNVERIFIED {totals.unverified}</span>{' '}
        <span style={statusPillStyle('no_policy')}>n/a {totals.no_policy}</span>
        {totals.error > 0 ? (
          <>
            {' '}
            <span style={statusPillStyle('error')}>ERROR {totals.error}</span>
          </>
        ) : null}
      </div>

      <table style={TABLE_STYLE}>
        <thead>
          <tr>
            <th style={TH_STYLE}>Task</th>
            <th style={TH_STYLE}>Verified</th>
            <th style={TH_STYLE}>Mismatch</th>
            <th style={TH_STYLE}>Unverifiable</th>
            <th style={TH_STYLE}>Status</th>
            <th style={TH_STYLE}></th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => {
            const isOpen = expanded === row.taskId
            return (
              <>
                <tr
                  key={row.taskId}
                  onClick={() =>
                    setExpanded(isOpen ? null : row.taskId)
                  }
                  style={{ cursor: 'pointer' }}
                  data-testid={`claims-row-${row.taskId}`}
                >
                  <td style={TD_STYLE}>
                    <code>{row.taskId}</code>
                  </td>
                  <td style={TD_STYLE}>
                    {row.report?.n_verified ?? '—'}
                  </td>
                  <td style={TD_STYLE}>
                    {row.report?.n_mismatch ?? '—'}
                  </td>
                  <td style={TD_STYLE}>
                    {row.report?.n_unverifiable ?? '—'}
                  </td>
                  <td style={TD_STYLE}>
                    <span style={statusPillStyle(row.status)}>
                      {labelFor(row.status)}
                    </span>
                  </td>
                  <td style={TD_STYLE}>
                    <button
                      type="button"
                      onClick={(e) => {
                        e.stopPropagation()
                        void reverify(row.taskId)
                      }}
                      disabled={row.verifying || row.status === 'no_policy'}
                      style={{
                        fontSize: 11,
                        padding: '2px 8px',
                        cursor: row.verifying ? 'wait' : 'pointer',
                      }}
                      title={
                        row.status === 'no_policy'
                          ? 'Task has no interpretation policy with verifiableEntities — nothing to verify.'
                          : 'Re-run claim_extractor + claim_verifier for this task'
                      }
                    >
                      {row.verifying ? 'Verifying…' : 'Re-verify'}
                    </button>
                  </td>
                </tr>
                {isOpen && row.report && row.report.verdicts.length > 0 ? (
                  <tr key={`${row.taskId}-detail`}>
                    <td colSpan={6} style={{ ...TD_STYLE, background: 'var(--color-surface-2, #f8fafc)' }}>
                      {row.report.runtime_decision_log_path ? (
                        <div style={{ marginBottom: 6, fontSize: 12 }}>
                          <a
                            href={`/artifacts/${row.report.runtime_decision_log_path}`}
                            target="_blank"
                            rel="noreferrer"
                            data-testid="runtime-decision-log-link"
                            style={{ color: '#1d4ed8', textDecoration: 'underline' }}
                          >
                            Runtime decision log:{' '}
                            <code>{row.report.runtime_decision_log_path}</code>
                          </a>
                        </div>
                      ) : null}
                      <VerdictsList verdicts={row.report.verdicts} />
                    </td>
                  </tr>
                ) : null}
                {isOpen && row.error ? (
                  <tr key={`${row.taskId}-err`}>
                    <td colSpan={6} style={{ ...TD_STYLE, color: '#991b1b' }}>
                      {row.error}
                    </td>
                  </tr>
                ) : null}
              </>
            )
          })}
        </tbody>
      </table>
    </div>
  )
}

function strengthBadgeStyle(strength: ClaimStrength): React.CSSProperties {
  const base: React.CSSProperties = {
    display: 'inline-block',
    padding: '0 5px',
    marginLeft: 6,
    borderRadius: 3,
    fontSize: 10,
    fontWeight: 600,
    letterSpacing: 0.3,
    textTransform: 'uppercase',
    verticalAlign: 'middle',
  }
  switch (strength) {
    case 'prespecified':
      return { ...base, background: '#dbeafe', color: '#1e3a8a' }
    case 'post_hoc':
      return { ...base, background: '#fee2e2', color: '#991b1b' }
    case 'exploratory':
      return { ...base, background: 'transparent', color: 'transparent' }
  }
}

function StrengthBadge({ strength }: { strength: ClaimStrength }): JSX.Element | null {
  // Exploratory is the no-op default (every claim in a non-confirmatory
  // session); rendering "EXPLORATORY" next to every verdict would be
  // visual noise without informing the SME. Suppress.
  if (strength === 'exploratory') return null
  const label = strength === 'prespecified' ? 'Prespecified' : 'Post-hoc'
  const title =
    strength === 'prespecified'
      ? 'Claim derives from a stage with no post-emission deviation — supports its prespecified analysis discipline.'
      : 'Claim derives from a stage that was amended after emission; treat conclusions as post-hoc rather than confirmatory.'
  return (
    <span
      data-testid={`strength-badge-${strength}`}
      style={strengthBadgeStyle(strength)}
      title={title}
    >
      {label}
    </span>
  )
}

function VerdictsList({ verdicts }: { verdicts: ClaimVerdict[] }): JSX.Element {
  return (
    <ul style={{ margin: 0, paddingLeft: 18, fontSize: 12 }}>
      {verdicts.map((v, i) => {
        const claim = v.claim
        const s = v.status
        const isMatch = s.status === 'verified'
        const isMismatch = s.status === 'mismatch'
        const detail = isMismatch ? s.detail : s.status === 'unverifiable' ? s.reason : null
        return (
          <li key={i} style={{ marginBottom: 4 }}>
            <span style={{ color: isMatch ? '#16a34a' : isMismatch ? '#dc2626' : 'var(--color-text-muted, #666)' }}>
              {isMatch ? '✓' : isMismatch ? '⚠' : '?'}
            </span>{' '}
            <code>{claim.entity}</code>
            {claim.direction ? ` ${claim.direction}` : ''}
            {claim.effect_size !== undefined && claim.effect_size !== null
              ? ` (effect=${claim.effect_size})`
              : ''}
            {claim.pvalue !== undefined && claim.pvalue !== null
              ? ` p=${claim.pvalue}`
              : ''}
            {' — '}
            <span style={{ color: 'var(--color-text-muted, #666)' }}>{s.status}</span>
            {detail ? <>: <span style={{ color: '#7c2d12' }}>{sanitizeForSme(detail)}</span></> : null}
            <StrengthBadge strength={v.strength ?? 'exploratory'} />
          </li>
        )
      })}
    </ul>
  )
}
