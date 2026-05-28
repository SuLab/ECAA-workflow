/**
 * `RepairProposalCard` UI component.
 *
 * Renders one `RepairProposal` produced by the planner's
 * repair-strategy wiring. Shows the strategy id, risk class, rationale,
 * any required credentials, and the assumptions the repair would
 * introduce. Three actions:
 *
 *  - **Accept** — POSTs to `/api/chat/session/:id/repair/:proposal_id/accept`
 *  with the SME's credential chain. High-risk proposals (
 *  `high_credentialed_review`) require the SME to type in a comma-
 *  separated credential list; lower-risk proposals dispatch with an
 *  empty list.
 *  - **Reject** — POSTs to `.../reject` with a free-text reason.
 *  - **Defer** — no network call; hides the card client-side so the SME
 *  can revisit later.
 *
 * F20 invariant: accepting a `medium_user_gated` or
 * `high_credentialed_review` proposal records a substrate
 * `RepairAccepted` row but does NOT itself mutate the DAG. The follow-
 * up planner re-run consumes the substrate; this card never edits the
 * DAG directly.
 */

import React, { useState } from 'react'
import { voidFetch } from '../../api/_fetch'
import type { RepairProposal } from '../../types/RepairProposal'
import type { RepairRiskClass } from '../../types/RepairRiskClass'

interface Props {
  /**
   * Session id. Optional — only consulted by the internal accept/reject
   * fetch paths when `onAccept` / `onReject` callbacks aren't provided.
   */
  sessionId?: string
  proposal: RepairProposal
  /**
   * Legacy single-callback shim. Fires after either the internal
   * accept- or reject-fetch resolves with a 2xx. Newer call sites pass
   * `onAccept` / `onReject` explicitly and ignore this.
   */
  onResolved?: () => void
  /**
   * When provided, the Accept button delegates to this callback instead
   * of issuing the internal fetch. The parent is responsible for the
   * server round-trip + any post-action refresh. Receives the proposal
   * id plus the comma-split credential chain the SME entered (empty
   * array when no credentials are required).
   */
  onAccept?: (proposalId: string, credentials: string[]) => Promise<void> | void
  /**
   * When provided, the Reject button delegates to this callback.
   * Receives the proposal id + the SME-supplied reason.
   */
  onReject?: (proposalId: string, reason: string) => Promise<void> | void
}

export function RepairProposalCard({
  sessionId,
  proposal,
  onResolved,
  onAccept,
  onReject,
}: Props) {
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [deferred, setDeferred] = useState(false)
  const [credentials, setCredentials] = useState<string>('')
  const [rejectReason, setRejectReason] = useState<string>('')
  const [mode, setMode] = useState<'idle' | 'reject'>('idle')

  if (deferred) {
    return null
  }

  async function accept() {
    setBusy(true)
    setError(null)
    try {
      const creds = credentials
        .split(',')
        .map((s) => s.trim())
        .filter((s) => s.length > 0)
      if (onAccept) {
        await onAccept(proposal.id, creds)
        if (onResolved) onResolved()
        return
      }
      if (!sessionId) {
        setError('Internal error: no sessionId and no onAccept handler.')
        return
      }
      const res = await fetch( // allow-bare-fetch: status-specific 403 handling for credentials-required
        `/api/chat/session/${sessionId}/repair/${proposal.id}/accept`,
        {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ credentials: creds }),
        },
      )
      if (res.status === 403) {
        setError(
          'Required credentials not provided. List the credential classes (comma-separated).',
        )
        return
      }
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`)
      }
      if (onResolved) onResolved()
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(false)
    }
  }

  async function reject() {
    if (rejectReason.trim().length === 0) {
      setError('Reason required.')
      return
    }
    setBusy(true)
    setError(null)
    try {
      if (onReject) {
        await onReject(proposal.id, rejectReason.trim())
        if (onResolved) onResolved()
        return
      }
      if (!sessionId) {
        setError('Internal error: no sessionId and no onReject handler.')
        return
      }
      await voidFetch(
        `/api/chat/session/${encodeURIComponent(sessionId)}/repair/${encodeURIComponent(proposal.id)}/reject`,
        {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ reason: rejectReason.trim() }),
        },
      )
      if (onResolved) onResolved()
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(false)
    }
  }

  const requiresCreds = proposal.required_credentials.length > 0
  // F20 / the HighCredentialedReview tier always requires an
  // explicit credential chain. Disable Accept until the SME types
  // something into the credentials field; lower tiers can accept with
  // empty credentials (subject to the proposal's required list).
  const credsBlank = credentials.trim().length === 0
  const acceptDisabled =
    busy ||
    (proposal.risk_class === 'high_credentialed_review' && credsBlank) ||
    (requiresCreds && credsBlank)

  return (
    <div style={cardStyle}>
      <header style={headerStyle}>
        <strong style={strategyStyle}>{proposal.strategy_id}</strong>
        <span style={riskBadgeStyle(proposal.risk_class)}>
          {riskClassLabel(proposal.risk_class)}
        </span>
      </header>
      <div style={rationaleStyle}>{proposal.rationale}</div>
      <div style={metaRowStyle}>
        <span style={metaLabelStyle}>Gap</span>
        <code style={codeStyle}>{proposal.gap_id}</code>
      </div>
      {requiresCreds && (
        <div style={metaRowStyle}>
          <span style={metaLabelStyle}>Required credentials</span>
          <ul style={chipListStyle}>
            {proposal.required_credentials.map((c) => (
              <li key={c} style={chipStyle}>
                {c}
              </li>
            ))}
          </ul>
        </div>
      )}
      {proposal.generated_assumptions.length > 0 && (
        <div style={metaRowStyle}>
          <span style={metaLabelStyle}>Introduces assumptions</span>
          <ul style={assumptionListStyle}>
            {proposal.generated_assumptions.map((a) => (
              <li key={a.id} style={assumptionItemStyle}>
                <code style={codeStyle}>{a.id}</code>
                <span>{a.statement}</span>
              </li>
            ))}
          </ul>
        </div>
      )}

      {requiresCreds && (
        <div style={inputRowStyle}>
          <label htmlFor={`creds-${proposal.id}`} style={metaLabelStyle}>
            Your credentials
          </label>
          <input
            id={`creds-${proposal.id}`}
            type="text"
            value={credentials}
            onChange={(e) => setCredentials(e.target.value)}
            placeholder="e.g. bioinformatics_lead, clinical_lead"
            style={inputStyle}
            disabled={busy}
          />
        </div>
      )}

      {mode === 'reject' && (
        <div style={inputRowStyle}>
          <label htmlFor={`reason-${proposal.id}`} style={metaLabelStyle}>
            Reason
          </label>
          <input
            id={`reason-${proposal.id}`}
            type="text"
            value={rejectReason}
            onChange={(e) => setRejectReason(e.target.value)}
            placeholder="why you're rejecting this proposal"
            style={inputStyle}
            disabled={busy}
          />
        </div>
      )}

      <footer style={footerStyle}>
        <button
          type="button"
          disabled={acceptDisabled}
          onClick={accept}
          style={primaryButton}
          aria-disabled={acceptDisabled}
          title={
            proposal.risk_class === 'high_credentialed_review' && credsBlank
              ? 'High-credentialed review tier: provide a credential chain before accepting.'
              : undefined
          }
        >
          {busy ? 'Accepting…' : 'Accept'}
        </button>
        {mode === 'idle' ? (
          <button
            type="button"
            disabled={busy}
            onClick={() => setMode('reject')}
            style={secondaryButton}
          >
            Reject
          </button>
        ) : (
          <button
            type="button"
            disabled={busy}
            onClick={reject}
            style={dangerButton}
          >
            {busy ? 'Rejecting…' : 'Confirm reject'}
          </button>
        )}
        <button
          type="button"
          disabled={busy}
          onClick={() => setDeferred(true)}
          style={tertiaryButton}
        >
          Defer
        </button>
      </footer>

      {error && <div style={errorStyle}>{error}</div>}
    </div>
  )
}

function riskClassLabel(rc: RepairRiskClass): string {
  switch (rc) {
    case 'low_auto_attempt':
      return 'low / auto-attempt'
    case 'medium_user_gated':
      return 'medium / user-gated'
    case 'high_credentialed_review':
      return 'high / credentialed review'
    default:
      return rc
  }
}

function riskBadgeStyle(rc: RepairRiskClass): React.CSSProperties {
  const base: React.CSSProperties = {
    fontSize: '0.7rem',
    padding: '0.1rem 0.4rem',
    borderRadius: '0.25rem',
    fontFamily: 'ui-monospace, monospace',
  }
  switch (rc) {
    case 'low_auto_attempt':
      return {
        ...base,
        background: 'var(--color-success-muted, #c8e6c9)',
        color: 'var(--color-success-fg, #1b5e20)',
      }
    case 'medium_user_gated':
      return {
        ...base,
        background: 'var(--color-warning-muted, #ffe0b2)',
        color: 'var(--color-warning-fg, #6f4d00)',
      }
    case 'high_credentialed_review':
      return {
        ...base,
        background: 'var(--color-danger-muted, #ffcdd2)',
        color: 'var(--color-danger-fg, #b71c1c)',
      }
    default:
      return base
  }
}

const cardStyle: React.CSSProperties = {
  padding: '0.6rem 0.8rem',
  border: '1px solid var(--color-border-subtle, #ccc)',
  borderRadius: '0.4rem',
  background: 'var(--color-surface-1, #f7f7f7)',
  marginTop: '0.5rem',
  fontSize: '0.78rem',
}
const headerStyle: React.CSSProperties = {
  display: 'flex',
  justifyContent: 'space-between',
  alignItems: 'center',
  marginBottom: '0.45rem',
}
const strategyStyle: React.CSSProperties = {
  fontWeight: 600,
  fontFamily: 'ui-monospace, monospace',
}
const rationaleStyle: React.CSSProperties = {
  fontSize: '0.74rem',
  color: 'var(--color-text-default, #222)',
  marginBottom: '0.4rem',
  lineHeight: 1.4,
}
const metaRowStyle: React.CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  gap: '0.4rem',
  marginBottom: '0.25rem',
  flexWrap: 'wrap',
}
const metaLabelStyle: React.CSSProperties = {
  minWidth: '8rem',
  fontWeight: 500,
  fontSize: '0.72rem',
  color: 'var(--color-text-muted, #555)',
}
const codeStyle: React.CSSProperties = {
  fontFamily: 'ui-monospace, monospace',
  background: 'var(--color-surface-2, #fff)',
  padding: '0.05rem 0.3rem',
  borderRadius: '0.2rem',
  fontSize: '0.72rem',
}
const chipListStyle: React.CSSProperties = {
  display: 'flex',
  flexWrap: 'wrap',
  gap: '0.25rem',
  listStyle: 'none',
  margin: 0,
  padding: 0,
}
const chipStyle: React.CSSProperties = {
  fontSize: '0.7rem',
  background: 'var(--color-surface-muted, #eaeaea)',
  padding: '0.1rem 0.4rem',
  borderRadius: '0.25rem',
  fontFamily: 'ui-monospace, monospace',
}
const assumptionListStyle: React.CSSProperties = {
  margin: 0,
  paddingLeft: '1rem',
  flex: 1,
}
const assumptionItemStyle: React.CSSProperties = {
  display: 'flex',
  flexDirection: 'column',
  gap: '0.15rem',
  marginBottom: '0.25rem',
  fontSize: '0.72rem',
}
const inputRowStyle: React.CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  gap: '0.4rem',
  marginTop: '0.4rem',
}
const inputStyle: React.CSSProperties = {
  flex: 1,
  minWidth: '12rem',
  padding: '0.25rem 0.4rem',
  fontSize: '0.74rem',
  border: '1px solid var(--color-border-subtle, #ccc)',
  borderRadius: '0.25rem',
}
const footerStyle: React.CSSProperties = {
  display: 'flex',
  gap: '0.5rem',
  marginTop: '0.55rem',
}
const primaryButton: React.CSSProperties = {
  padding: '0.3rem 0.65rem',
  fontSize: '0.72rem',
  background: 'var(--color-success-accent, #2e7d32)',
  color: '#fff',
  border: 'none',
  borderRadius: '0.25rem',
  cursor: 'pointer',
}
const secondaryButton: React.CSSProperties = {
  padding: '0.3rem 0.65rem',
  fontSize: '0.72rem',
  background: 'var(--color-surface-muted, #eaeaea)',
  color: 'var(--color-text-default, #222)',
  border: '1px solid var(--color-border-subtle, #ccc)',
  borderRadius: '0.25rem',
  cursor: 'pointer',
}
const tertiaryButton: React.CSSProperties = {
  padding: '0.3rem 0.65rem',
  fontSize: '0.72rem',
  background: 'transparent',
  color: 'var(--color-text-muted, #555)',
  border: '1px solid var(--color-border-subtle, #ccc)',
  borderRadius: '0.25rem',
  cursor: 'pointer',
}
const dangerButton: React.CSSProperties = {
  padding: '0.3rem 0.65rem',
  fontSize: '0.72rem',
  background: 'var(--color-danger-fg, #b71c1c)',
  color: '#fff',
  border: 'none',
  borderRadius: '0.25rem',
  cursor: 'pointer',
}
const errorStyle: React.CSSProperties = {
  marginTop: '0.4rem',
  fontSize: '0.7rem',
  color: 'var(--color-danger-fg, #b71c1c)',
}

export default RepairProposalCard
