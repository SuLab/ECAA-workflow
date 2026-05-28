// Hypothesized-node proposal progress card.
//
// Mounted inline in the chat scroll once a proposal exists on the
// session (one card per non-terminal proposal). Renders the three-gate
// promotion-pipeline progress strip (validator → sandbox → SME signoff)
// and the SME action buttons (Approve & promote / Reject). Live state
// flows from `useSseChatEvents.proposalEvents` keyed by proposal id;
// the chip state resolves from the SSE overlay first, then the
// authoritative `proposal.gate_outcomes` and `proposal.lifecycle`
// position. Pattern mirrors `BlockerCard.tsx` — turn-card shape,
// `useAsync` lifecycle, semantic markup, inline buttons.
//
// Lifecycle states (`proposal.lifecycle.kind`):
//
// pending_validation → pending_sandbox → awaiting_signoff → promoted
// │
// ↓ (any gate failure or SME reject)
// blocked / rejected

import { useState } from 'react'
import type { GateName } from '../types/GateName'
import type { GateOutcome } from '../types/GateOutcome'
import type { HypothesizedProposal } from '../types/HypothesizedProposal'
import type { ProposalBlockerReason } from '../types/ProposalBlockerReason'
import type { ProposalLifecycle } from '../types/ProposalLifecycle'
import { rejectProposal, signoffProposal } from '../api/chatClient'
import { useAsync } from '../hooks/useAsync'
import type { ProposalEventState } from '../hooks/useSseChatEvents'

export interface HypothesizedProposalCardProps {
  sessionId: string
  proposal: HypothesizedProposal
  /**
   * Live SSE overlay keyed by `proposal_id`; when absent the card
   * reads chip state from `proposal.gate_outcomes` + lifecycle.
   */
  liveOverlay?: ProposalEventState | null
  /**
   * Fired after a successful signoff so the parent can refresh the
   * proposal list / DAG.
   */
  onPromoted?: (taskNodeId: string) => void
  /**
   * Fired after a successful reject.
   */
  onRejected?: () => void
}

type ChipStatus = 'running' | 'passed' | 'failed' | 'pending'

interface ChipModel {
  gate: GateName
  label: string
  status: ChipStatus
}

const GATE_ORDER: GateName[] = ['validator', 'sandbox', 'sme_signoff']

const GATE_LABEL: Record<GateName, string> = {
  validator: 'Validator',
  sandbox: 'Sandbox',
  sme_signoff: 'SME signoff',
}

const CHIP_ICON: Record<ChipStatus, string> = {
  running: '⏳', // ⏳
  passed: '✓', // ✓
  failed: '✗', // ✗
  pending: '⏸', // ⏸
}

const CHIP_DESCRIPTION: Record<ChipStatus, string> = {
  running: 'Running',
  passed: 'Passed',
  failed: 'Failed',
  pending: 'Pending',
}

/// Resolve the chip status for one gate, with SSE overlay taking
/// precedence over the REST-derived `gate_outcomes` snapshot. The
/// overlay is what makes the chip flip immediately when the SSE
/// `proposal_gate_advanced` event lands without waiting for a REST
/// refetch.
function chipFor(
  gate: GateName,
  lifecycle: ProposalLifecycle,
  outcomes: GateOutcome[],
  overlay: ProposalEventState | null | undefined,
): ChipModel {
  const overlaid =
    overlay != null
      ? gate === 'validator'
        ? overlay.validator
        : gate === 'sandbox'
          ? overlay.sandbox
          : undefined
      : undefined
  if (overlaid !== undefined) {
    return {
      gate,
      label: GATE_LABEL[gate],
      status: overlaid ? 'passed' : 'failed',
    }
  }
  // For the SME signoff gate, the overlay carries the terminal flag.
  if (gate === 'sme_signoff' && overlay?.terminal === 'promoted') {
    return { gate, label: GATE_LABEL[gate], status: 'passed' }
  }
  if (gate === 'sme_signoff' && overlay?.terminal === 'rejected') {
    return { gate, label: GATE_LABEL[gate], status: 'failed' }
  }

  const recorded = outcomes.find((o) => o.gate === gate)
  if (recorded) {
    return {
      gate,
      label: GATE_LABEL[gate],
      status: recorded.passed ? 'passed' : 'failed',
    }
  }

  // Lifecycle-position fallback when neither overlay nor recorded
  // outcome exists.
  switch (lifecycle.kind) {
    case 'pending_validation':
      if (gate === 'validator') return { gate, label: GATE_LABEL[gate], status: 'running' }
      return { gate, label: GATE_LABEL[gate], status: 'pending' }
    case 'pending_sandbox':
      if (gate === 'validator') return { gate, label: GATE_LABEL[gate], status: 'passed' }
      if (gate === 'sandbox') return { gate, label: GATE_LABEL[gate], status: 'running' }
      return { gate, label: GATE_LABEL[gate], status: 'pending' }
    case 'awaiting_signoff':
      if (gate === 'sme_signoff') return { gate, label: GATE_LABEL[gate], status: 'running' }
      return { gate, label: GATE_LABEL[gate], status: 'passed' }
    case 'promoted':
      return { gate, label: GATE_LABEL[gate], status: 'passed' }
    case 'blocked':
      // Map the blocker reason back onto the offending gate.
      if (lifecycle.reason.kind === 'validator_failed' && gate === 'validator') {
        return { gate, label: GATE_LABEL[gate], status: 'failed' }
      }
      if (lifecycle.reason.kind === 'sandbox_refused' && gate === 'sandbox') {
        return { gate, label: GATE_LABEL[gate], status: 'failed' }
      }
      if (lifecycle.reason.kind === 'materialization_failed' && gate === 'sme_signoff') {
        return { gate, label: GATE_LABEL[gate], status: 'failed' }
      }
      if (lifecycle.reason.kind === 'sme_rejected' && gate === 'sme_signoff') {
        return { gate, label: GATE_LABEL[gate], status: 'failed' }
      }
      return { gate, label: GATE_LABEL[gate], status: 'pending' }
    case 'rejected':
      if (gate === 'sme_signoff') return { gate, label: GATE_LABEL[gate], status: 'failed' }
      // Upstream gates may have completed before the reject; if
      // recorded above we'd have returned already. Default to pending.
      return { gate, label: GATE_LABEL[gate], status: 'pending' }
  }
  const _exhaustive: never = lifecycle
  void _exhaustive
  return { gate, label: GATE_LABEL[gate], status: 'pending' }
}

/// Human-readable summary of a `ProposalBlockerReason` for the inline
/// alert. Empty refusal / failure lists fall back to a generic
/// sentence.
function blockerReasonCopy(reason: ProposalBlockerReason): string {
  switch (reason.kind) {
    case 'validator_failed':
      if (reason.failures.length === 0) {
        return 'Validator failed — see details below.'
      }
      return `Validator failed: ${reason.failures.join(', ')}`
    case 'sandbox_refused':
      if (reason.refusals.length === 0) {
        return 'Sandbox refused — see details below.'
      }
      return `Sandbox refused: ${reason.refusals.map((r) => r.kind).join(', ')}`
    case 'materialization_failed':
      return `Materialization failed: ${reason.reason}`
    case 'sme_rejected':
      return reason.rationale
        ? `Rejected: ${reason.rationale}`
        : 'Rejected by SME.'
  }
  const _exhaustive: never = reason
  void _exhaustive
  return 'This proposal is blocked.'
}

/// `gate_outcomes` may carry multiple entries per gate over a
/// proposal's lifetime (e.g. retries). The card shows the latest by
/// `recorded_at`. Filter helper used by the collapsible detail
/// sections.
function latestOutcomeFor(outcomes: GateOutcome[], gate: GateName): GateOutcome | null {
  const filtered = outcomes.filter((o) => o.gate === gate)
  if (filtered.length === 0) return null
  return filtered.reduce((a, b) => (a.recorded_at >= b.recorded_at ? a : b))
}

const STYLES = {
  section: {
    marginTop: '0.75rem',
    padding: '0.85rem 1rem',
    background: 'var(--color-info-bg)',
    border: '1px solid #93c5fd',
    borderLeft: '4px solid #2563eb',
    borderRadius: 8,
    display: 'flex',
    flexDirection: 'column',
    gap: '0.55rem',
  } as const,
  collapsedSection: {
    marginTop: '0.75rem',
    padding: '0.55rem 0.85rem',
    background: 'var(--color-surface-2)',
    border: '1px solid var(--color-border-default)',
    borderRadius: 8,
    color: 'var(--color-text-faint)',
    fontSize: '0.8rem',
    display: 'flex',
    gap: '0.4rem',
    alignItems: 'center',
    flexWrap: 'wrap' as const,
  } as const,
  heading: {
    margin: 0,
    fontSize: '0.92rem',
    color: 'var(--color-info-fg)',
    fontWeight: 600,
  } as const,
  parentLine: {
    margin: 0,
    fontSize: '0.78rem',
    color: 'var(--color-text-secondary)',
    fontFamily: 'ui-monospace, monospace',
  } as const,
  chipStrip: {
    display: 'flex',
    gap: '0.3rem',
    alignItems: 'center',
    flexWrap: 'wrap' as const,
    padding: '0.35rem 0',
  } as const,
  chipSeparator: {
    color: 'var(--color-text-faint)',
    fontSize: '0.9rem',
  } as const,
  intent: {
    margin: 0,
    fontSize: '0.83rem',
    color: 'var(--color-text-primary)',
    lineHeight: 1.5,
  } as const,
  rationale: {
    margin: 0,
    fontSize: '0.78rem',
    color: 'var(--color-text-secondary)',
    fontStyle: 'italic' as const,
    lineHeight: 1.45,
  } as const,
  detailSummary: {
    fontSize: '0.76rem',
    color: 'var(--color-info-fg)',
    fontWeight: 500,
    cursor: 'pointer',
  } as const,
  detailRow: {
    fontSize: '0.74rem',
    color: 'var(--color-text-primary)',
    fontFamily: 'ui-monospace, monospace',
    lineHeight: 1.45,
  } as const,
  buttonRow: {
    display: 'flex',
    gap: '0.4rem',
    marginTop: '0.35rem',
    flexWrap: 'wrap' as const,
  } as const,
  primaryButton: {
    padding: '0.45rem 0.9rem',
    background: 'var(--color-info-accent)',
    color: 'var(--color-text-on-accent)',
    border: 'none',
    borderRadius: 6,
    fontSize: '0.8rem',
    fontWeight: 600,
    cursor: 'pointer',
  } as const,
  secondaryButton: {
    padding: '0.45rem 0.9rem',
    background: 'transparent',
    color: 'var(--color-info-fg)',
    border: '1px solid #93c5fd',
    borderRadius: 6,
    fontSize: '0.8rem',
    fontWeight: 500,
    cursor: 'pointer',
  } as const,
  rationaleTextarea: {
    width: '100%',
    marginTop: 4,
    padding: '0.4rem 0.5rem',
    borderRadius: 4,
    border: '1px solid #93c5fd',
    fontSize: '0.78rem',
    fontFamily: 'inherit',
    background: 'var(--color-info-bg)',
    resize: 'vertical' as const,
    boxSizing: 'border-box' as const,
  } as const,
  errorText: {
    margin: 0,
    fontSize: '0.76rem',
    color: 'var(--color-danger-fg)',
  } as const,
  alertBox: {
    marginTop: '0.3rem',
    padding: '0.5rem 0.7rem',
    borderRadius: 6,
    background: 'var(--color-warning-bg)',
    border: '1px solid var(--color-warning-border)',
    color: 'var(--color-warning-fg)',
    fontSize: '0.8rem',
    lineHeight: 1.45,
  } as const,
}

/// Chip-status colour palette. The three runtime states use distinct
/// background tokens so chips are distinguishable in colour-blind
/// modes via shape (icon) AND tone.
const CHIP_PALETTE: Record<
  ChipStatus,
  { bg: string; fg: string; border: string }
> = {
  running: {
    bg: 'var(--color-warning-bg)',
    fg: 'var(--color-warning-fg)',
    border: 'var(--color-warning-border)',
  },
  passed: {
    bg: 'var(--color-success-bg)',
    fg: 'var(--color-success-fg)',
    border: 'var(--color-success-border)',
  },
  failed: {
    bg: 'var(--color-danger-bg)',
    fg: 'var(--color-danger-fg)',
    border: 'var(--color-danger-border)',
  },
  pending: {
    bg: 'var(--color-surface-2)',
    fg: 'var(--color-text-faint)',
    border: 'var(--color-border-default)',
  },
}

function GateChip({ chip }: { chip: ChipModel }): JSX.Element {
  const palette = CHIP_PALETTE[chip.status]
  return (
    <span
      data-testid={`proposal-chip-${chip.gate}`}
      data-chip-status={chip.status}
      aria-label={`${chip.label}: ${CHIP_DESCRIPTION[chip.status]}`}
      style={{
        display: 'inline-flex',
        alignItems: 'center',
        gap: '0.3rem',
        padding: '0.15rem 0.55rem',
        background: palette.bg,
        color: palette.fg,
        border: `1px solid ${palette.border}`,
        borderRadius: 999,
        fontSize: '0.74rem',
        fontWeight: 600,
        whiteSpace: 'nowrap',
      }}
    >
      <span aria-hidden="true">{CHIP_ICON[chip.status]}</span>
      <span>{chip.label}</span>
    </span>
  )
}

export default function HypothesizedProposalCard(
  props: HypothesizedProposalCardProps,
): JSX.Element {
  const { sessionId, proposal, liveOverlay, onPromoted, onRejected } = props
  const { busy, error, run } = useAsync()
  const [rejectingMode, setRejectingMode] = useState(false)
  const [rejectRationale, setRejectRationale] = useState('')

  const lifecycle = proposal.lifecycle
  const terminal = liveOverlay?.terminal ?? null

  // Terminal-state collapse: SSE overlay flips immediately on
  // `proposal_promoted` / `proposal_rejected`; if no overlay has
  // arrived yet we fall through to the REST-snapshot `lifecycle`.
  const isPromoted =
    terminal === 'promoted' || lifecycle.kind === 'promoted'
  const isRejected =
    terminal === 'rejected' || lifecycle.kind === 'rejected'

  if (isPromoted) {
    const taskNodeId =
      liveOverlay?.promotedTaskNodeId ??
      (lifecycle.kind === 'promoted' ? lifecycle.task_node_id : null)
    return (
      <section
        role="status"
        aria-label={`Proposal ${proposal.node_id} promoted`}
        data-proposal-id={proposal.id}
        data-proposal-state="promoted"
        style={STYLES.collapsedSection}
      >
        <strong style={{ color: 'var(--color-success-fg)' }}>
          {CHIP_ICON.passed} Promoted
        </strong>
        <span>
          as <code style={{ fontFamily: 'ui-monospace, monospace' }}>{taskNodeId ?? proposal.node_id}</code>
        </span>
      </section>
    )
  }

  if (isRejected) {
    const rationale =
      liveOverlay?.rejectRationale ??
      (lifecycle.kind === 'rejected' ? lifecycle.rationale ?? null : null)
    return (
      <section
        role="status"
        aria-label={`Proposal ${proposal.node_id} rejected`}
        data-proposal-id={proposal.id}
        data-proposal-state="rejected"
        title={rationale ?? undefined}
        style={STYLES.collapsedSection}
      >
        <strong style={{ color: 'var(--color-danger-fg)' }}>
          {CHIP_ICON.failed} Rejected
        </strong>
        {rationale && (
          <span style={{ fontStyle: 'italic' }}>— {rationale}</span>
        )}
      </section>
    )
  }

  const chips = GATE_ORDER.map((g) =>
    chipFor(g, lifecycle, proposal.gate_outcomes, liveOverlay),
  )
  const validatorOutcome = latestOutcomeFor(proposal.gate_outcomes, 'validator')
  const sandboxOutcome = latestOutcomeFor(proposal.gate_outcomes, 'sandbox')

  const canApprove = lifecycle.kind === 'awaiting_signoff' && !busy
  const isTerminal = isPromoted || isRejected
  // Spec §9: "Reject enabled while not terminal." Blocked is
  // non-terminal per the state machine (§4) — the SME may either
  // re-propose (via the inline alert hint) or reject outright.
  const canReject = !isTerminal && !busy

  const handleApprove = async () => {
    if (!canApprove) return
    await run(async () => {
      // SME initials default to "sme" on the server; the initials
      // prompt is intentionally skipped — the server side handles
      // the default and the audit log carries the proposal id +
      // timestamp regardless.
      await signoffProposal(sessionId, proposal.id)
      const promotedTaskNodeId =
        liveOverlay?.promotedTaskNodeId ?? proposal.node_id
      onPromoted?.(promotedTaskNodeId)
    })
  }

  const handleRejectSubmit = async () => {
    await run(async () => {
      const rationale = rejectRationale.trim() || undefined
      await rejectProposal(sessionId, proposal.id, rationale)
      onRejected?.()
    })
  }

  return (
    <section
      role="region"
      aria-labelledby={`proposal-heading-${proposal.id}`}
      data-proposal-id={proposal.id}
      data-proposal-state={lifecycle.kind}
      style={STYLES.section}
    >
      <h3 id={`proposal-heading-${proposal.id}`} style={STYLES.heading}>
        Proposed node:{' '}
        <code style={{ fontFamily: 'ui-monospace, monospace' }}>
          {proposal.node_id}
        </code>
      </h3>

      {proposal.parent_terms.length > 0 && (
        <p style={STYLES.parentLine}>
          Parent: {proposal.parent_terms.join(', ')}
        </p>
      )}

      <div
        role="status"
        aria-live="polite"
        aria-label="Proposal gate progress"
        data-testid="proposal-chip-strip"
        style={STYLES.chipStrip}
      >
        {chips.map((chip, idx) => (
          <span
            key={chip.gate}
            style={{ display: 'inline-flex', alignItems: 'center', gap: '0.3rem' }}
          >
            <GateChip chip={chip} />
            {idx < chips.length - 1 && (
              <span aria-hidden="true" style={STYLES.chipSeparator}>
                ─
              </span>
            )}
          </span>
        ))}
      </div>

      <p style={STYLES.intent}>
        <strong>Intent:</strong> {proposal.intent}
      </p>

      {proposal.llm_rationale && (
        <p style={STYLES.rationale}>
          <strong>Rationale:</strong> {proposal.llm_rationale}
        </p>
      )}

      {validatorOutcome && validatorOutcome.details.length > 0 && (
        <details data-testid="validator-details">
          <summary style={STYLES.detailSummary}>Validator details</summary>
          <ul
            style={{
              margin: '0.3rem 0 0',
              paddingLeft: '1.1rem',
              listStyle: 'none',
            }}
          >
            {validatorOutcome.details.map((d, i) => (
              <li key={i} style={STYLES.detailRow}>
                <span
                  aria-hidden="true"
                  style={{
                    marginRight: 6,
                    color: validatorOutcome.passed
                      ? 'var(--color-success-fg)'
                      : 'var(--color-danger-fg)',
                  }}
                >
                  {validatorOutcome.passed ? CHIP_ICON.passed : CHIP_ICON.failed}
                </span>
                {d}
              </li>
            ))}
          </ul>
        </details>
      )}

      {sandboxOutcome && sandboxOutcome.details.length > 0 && (
        <details data-testid="sandbox-details">
          <summary style={STYLES.detailSummary}>Sandbox details</summary>
          <ul
            style={{
              margin: '0.3rem 0 0',
              paddingLeft: '1.1rem',
              listStyle: 'none',
            }}
          >
            {sandboxOutcome.details.map((d, i) => (
              <li key={i} style={STYLES.detailRow}>
                <span
                  aria-hidden="true"
                  style={{
                    marginRight: 6,
                    color: sandboxOutcome.passed
                      ? 'var(--color-success-fg)'
                      : 'var(--color-danger-fg)',
                  }}
                >
                  {sandboxOutcome.passed ? CHIP_ICON.passed : CHIP_ICON.failed}
                </span>
                {d}
              </li>
            ))}
          </ul>
        </details>
      )}

      {lifecycle.kind === 'blocked' && (
        <div
          role="alert"
          data-testid="proposal-blocked-alert"
          data-blocker-reason={lifecycle.reason.kind}
          style={STYLES.alertBox}
        >
          <strong>This proposal is blocked.</strong>{' '}
          {blockerReasonCopy(lifecycle.reason)}
          <p
            style={{
              margin: '0.35rem 0 0',
              fontSize: '0.74rem',
              fontStyle: 'italic',
            }}
          >
            Re-propose: ask the assistant in chat for an updated proposal that
            addresses the failure above.
          </p>
        </div>
      )}

      {error && <p style={STYLES.errorText}>{error}</p>}

      {rejectingMode && !isTerminal && (
        <label
          style={{
            display: 'block',
            fontSize: '0.74rem',
            color: 'var(--color-info-fg)',
            fontWeight: 500,
          }}
        >
          Reason (optional, saved to the audit log)
          <textarea
            data-testid="reject-rationale"
            value={rejectRationale}
            onChange={(e) => setRejectRationale(e.target.value)}
            rows={2}
            placeholder="A short note on why this proposal isn't a fit."
            style={STYLES.rationaleTextarea}
          />
        </label>
      )}

      <div style={STYLES.buttonRow}>
        <button
          type="button"
          data-testid="proposal-approve"
          aria-label={`Approve and promote node ${proposal.node_id}`}
          disabled={!canApprove}
          onClick={handleApprove}
          style={{
            ...STYLES.primaryButton,
            opacity: canApprove ? 1 : 0.6,
            cursor: canApprove ? 'pointer' : 'not-allowed',
          }}
        >
          {busy && canApprove ? 'Promoting…' : 'Approve & promote'}
        </button>
        {rejectingMode ? (
          <>
            <button
              type="button"
              data-testid="proposal-reject-confirm"
              aria-label={`Confirm reject of proposal ${proposal.node_id}`}
              disabled={busy}
              onClick={handleRejectSubmit}
              style={{
                ...STYLES.secondaryButton,
                opacity: busy ? 0.6 : 1,
                cursor: busy ? 'not-allowed' : 'pointer',
              }}
            >
              {busy ? 'Rejecting…' : 'Confirm reject'}
            </button>
            <button
              type="button"
              data-testid="proposal-reject-cancel"
              aria-label="Cancel reject"
              disabled={busy}
              onClick={() => {
                setRejectingMode(false)
                setRejectRationale('')
              }}
              style={{
                ...STYLES.secondaryButton,
                opacity: busy ? 0.6 : 1,
                cursor: busy ? 'not-allowed' : 'pointer',
              }}
            >
              Cancel
            </button>
          </>
        ) : (
          <button
            type="button"
            data-testid="proposal-reject"
            aria-label={`Reject proposal ${proposal.node_id}`}
            disabled={!canReject}
            onClick={() => setRejectingMode(true)}
            style={{
              ...STYLES.secondaryButton,
              opacity: canReject ? 1 : 0.6,
              cursor: canReject ? 'pointer' : 'not-allowed',
            }}
          >
            Reject
          </button>
        )}
      </div>
    </section>
  )
}
