import { useState } from 'react'
import type {
  ConfirmationCard,
  ProjectClass,
  ResourceEstimate,
  SessionMode,
  CheckpointMode,
} from '../types'
import { CardContainer } from './primitives/CardContainer'
import { SubmitCancelRow } from './primitives/SubmitCancelRow'
import ClinicalConfirmGate from './ClinicalConfirmGate'
import { setPolicyBundle } from '../api/chatClient'
import { useSessionContext } from '../hooks/contexts'
import { formatUSD } from '../lib/format'

/**
 * Pre-Accept resource preview. Renders the composer's
 * coarse estimate as a single inline strip ("≈ N core-hours, peak M
 * GB, K GPU tasks") so the SME sees the cost envelope before clicking
 * Accept. Falls back to a single bullet line when the estimate is
 * empty (composer didn't surface a resource_estimate, or every atom
 * has no resource_profile).
 */
function ResourceEstimateChip({ estimate }: { estimate: ResourceEstimate }) {
  const parts: string[] = []
  if (estimate.total_core_hours > 0) {
    parts.push(`≈ ${estimate.total_core_hours.toFixed(1)} core-hours`)
  }
  if (estimate.peak_memory_gb > 0) {
    parts.push(`peak ${estimate.peak_memory_gb} GB RAM`)
  }
  if (estimate.gpu_task_count > 0) {
    parts.push(
      `${estimate.gpu_task_count} GPU task${estimate.gpu_task_count === 1 ? '' : 's'}`,
    )
  }
  if (estimate.estimated_cost_usd && estimate.estimated_cost_usd > 0) {
    parts.push(`≈ ${formatUSD(estimate.estimated_cost_usd)}`)
  }
  if (parts.length === 0) {
    return null
  }
  return (
    <div
      aria-label="Resource estimate"
      style={{
        marginTop: '0.6rem',
        padding: '0.35rem 0.55rem',
        fontSize: '0.74rem',
        color: 'var(--color-text-secondary)',
        backgroundColor: 'var(--color-surface-muted)',
        borderRadius: '0.25rem',
        fontFamily:
          'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace',
      }}
    >
      {parts.join(' · ')}
    </div>
  )
}

type DisciplineChoice = 'unset' | 'exploratory' | 'confirmatory'
type CheckpointChoice = 'gated' | 'selective' | 'fast'

interface Props {
  card: ConfirmationCard
  onConfirm: (opts?: { mode?: SessionMode; checkpointMode?: CheckpointMode }) => void | Promise<void>
  onReject: () => void | Promise<void>
  disabled?: boolean
  /** When `ClinicalTrial`, the Analysis discipline dropdown is
   *  required and Accept stays disabled until the SME picks. */
  projectClass?: ProjectClass
}

export default function ConfirmationTurnCard({
  card,
  onConfirm,
  onReject,
  disabled,
  projectClass = 'bioinformatics',
}: Props) {
  const sessionCtx = useSessionContext()
  const [decision, setDecision] = useState<'pending' | 'confirmed' | 'rejected'>(
    'pending',
  )
  // ClinicalTrial starts 'unset' so Accept is gated until the SME
  // picks. Bio + TimeSeries default Exploratory (optional dropdown).
  const [discipline, setDiscipline] = useState<DisciplineChoice>(
    projectClass === 'clinical_trial' ? 'unset' : 'exploratory',
  )
  const [checkpoint, setCheckpoint] = useState<CheckpointChoice>('gated')
  // ClinicalConfirmGate adds an additional regulatory
  // confirmation gate for clinical workflows. SME must acknowledge
  // every active policy obligation and type their initials before
  // Accept becomes available. Local-only (no network round-trip);
  // we capture the initials so the rationale string sent on
  // /confirm carries the typed-initials audit.
  const [clinicalInitials, setClinicalInitials] = useState<string | null>(
    null,
  )
  const requiresClinicalGate = projectClass === 'clinical_trial'
  const clinicalGateSatisfied =
    !requiresClinicalGate || clinicalInitials !== null

  const acceptGated =
    disabled ||
    decision !== 'pending' ||
    (projectClass === 'clinical_trial' && discipline === 'unset') ||
    !clinicalGateSatisfied

  const buildMode = (): SessionMode | undefined => {
    if (discipline === 'unset') return undefined
    if (discipline === 'confirmatory') {
      return { kind: 'confirmatory', prespecified_stages: [] }
    }
    return { kind: 'exploratory' }
  }

  const handleConfirm = async () => {
    if (acceptGated) return
    setDecision('confirmed')
    try {
      await onConfirm({
        mode: buildMode(),
        checkpointMode: checkpoint,
      })
    } catch {
      setDecision('pending')
    }
  }

  const handleReject = async () => {
    if (disabled || decision !== 'pending') return
    // Revise is a non-terminal action: the SME is sending the conversation
    // back into the loop, not committing to a decision. Show the "returning"
    // indicator transiently while onReject resolves, then reset to pending
    // so the card never gets stuck in a non-pending state. (S5.11 regression
    // guard — see ConfirmationTurnCard.test.tsx.)
    setDecision('rejected')
    try {
      await onReject()
    } finally {
      setDecision('pending')
    }
  }

  return (
    <CardContainer
      palette="neutral"
      ariaLabel="Plan summary — your confirmation"
    >
      <div
        style={{
          fontSize: '0.83rem',
          color: 'var(--color-text-primary)',
          whiteSpace: 'pre-wrap',
          lineHeight: 1.55,
        }}
      >
        {card.summary_markdown}
      </div>
      {/* Phase 10 / F-LLM-H3 — render the leading 12 hex chars of the
          SHA-256 fingerprint of the summary the SME is about to confirm.
          The same fingerprint lands on the durable `DecisionType::Confirm`
          audit record, so a later replayer can verify the displayed text
          matches the confirmation in `runtime/decisions.jsonl`. Empty
          string for legacy cards that pre-date the field. */}
      {card.summary_hash && card.summary_hash.length > 0 && (
        <div
          aria-label="Summary fingerprint"
          style={{
            marginTop: '0.6rem',
            fontSize: '0.68rem',
            color: 'var(--color-text-secondary)',
            fontFamily:
              'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace',
          }}
        >
          Summary fingerprint: {card.summary_hash.slice(0, 12)}
        </div>
      )}
      {card.resource_estimate && (
        <ResourceEstimateChip estimate={card.resource_estimate} />
      )}
      {decision === 'pending' && (
        <div style={{ marginTop: '0.85rem' }}>
          <DisciplineDropdown
            projectClass={projectClass}
            value={discipline}
            onChange={setDiscipline}
            disabled={disabled}
          />
          <CheckpointDropdown
            value={checkpoint}
            mode={buildMode()}
            onChange={setCheckpoint}
            disabled={disabled}
          />
          {requiresClinicalGate && !clinicalGateSatisfied && (
            <div style={{ marginTop: '0.7rem' }}>
              <ClinicalConfirmGate
                bundle_id="clinical_trial"
                bundle_label="Clinical-trial workflow obligations"
                bundle_description="This session is classified as a clinical trial. Before Accept becomes available, confirm every regulatory obligation listed below."
                obligations={[
                  'validated_nodes_only',
                  'require_pinned_containers',
                  'no_generated_code',
                  'no_policy_restricted_adapters',
                  'pinned_reference_data_only',
                  'audit_trail_required',
                  'human_signoff_required',
                ]}
                regulatory_citation="21 CFR Part 11 / FDA bioinformatics SOP"
                on_confirmed={(initials) => {
                  setClinicalInitials(initials)
                  // Actually activate the clinical_trial
                  // policy bundle on the session so the next compose
                  // run fires the per-node policy gate.
                  if (sessionCtx?.sessionId) {
                    void setPolicyBundle(
                      sessionCtx.sessionId,
                      'clinical_trial',
                    ).catch((err) => {
                      // Soft-fail: gate UX still proceeds; the
                      // missed activation surfaces as empty
                      // policy_decisions rather than a hard error.
                      console.warn(
                        'failed to activate clinical_trial policy bundle:',
                        err,
                      )
                    })
                  }
                }}
                on_canceled={() => {
                  setClinicalInitials(null)
                  void handleReject()
                }}
              />
            </div>
          )}
          {requiresClinicalGate && clinicalGateSatisfied && (
            <div
              role="status"
              style={{
                marginTop: '0.6rem',
                padding: '0.4rem 0.7rem',
                fontSize: '0.78rem',
                background: 'var(--color-surface-muted)',
                borderLeft: '3px solid #1f6f3a',
                borderRadius: '0.3rem',
              }}
            >
              Clinical obligations confirmed — initials: <strong>{clinicalInitials}</strong>.
            </div>
          )}
          <div style={{ marginTop: '0.6rem' }}>
            <SubmitCancelRow
              palette="neutral"
              submitLabel="Accept"
              cancelLabel="Revise"
              onSubmit={handleConfirm}
              onCancel={handleReject}
              disabled={acceptGated}
            />
          </div>
        </div>
      )}
      {decision === 'confirmed' && (
        <div
          aria-live="polite"
          style={{
            marginTop: '0.7rem',
            fontSize: '0.78rem',
            color: 'var(--color-success-fg)',
            fontWeight: 500,
          }}
        >
          Accepted — continuing.
        </div>
      )}
      {decision === 'rejected' && (
        <div
          aria-live="polite"
          style={{
            marginTop: '0.7rem',
            fontSize: '0.78rem',
            color: 'var(--color-warning-fg)',
            fontWeight: 500,
          }}
        >
          Returning to the conversation…
        </div>
      )}
    </CardContainer>
  )
}

function DisciplineDropdown({
  projectClass,
  value,
  onChange,
  disabled,
}: {
  projectClass: ProjectClass
  value: DisciplineChoice
  onChange: (v: DisciplineChoice) => void
  disabled?: boolean
}) {
  const isRequired = projectClass === 'clinical_trial'
  const labelStyle: React.CSSProperties = {
    display: 'block',
    fontSize: '0.78rem',
    color: 'var(--color-text-secondary)',
    fontWeight: 500,
    marginBottom: '0.3rem',
  }
  return (
    <label style={{ display: 'block', marginBottom: '0.55rem' }}>
      <span style={labelStyle}>
        Analysis discipline
        {isRequired && (
          <span
            aria-label="required"
            style={{ color: 'var(--color-danger-accent)', marginLeft: '0.25rem' }}
          >
            *
          </span>
        )}
      </span>
      <select
        value={value}
        onChange={(e) => onChange(e.target.value as DisciplineChoice)}
        disabled={disabled}
        aria-required={isRequired}
        style={{
          fontSize: '0.83rem',
          padding: '0.3rem 0.45rem',
          borderRadius: '0.35rem',
          border: '1px solid var(--color-border-strong)',
          background: 'var(--color-surface-1)',
          color: 'var(--color-text-primary)',
          minWidth: '18rem',
        }}
      >
        {isRequired && (
          <option value="unset" disabled>
            — pick one —
          </option>
        )}
        <option value="exploratory">Exploratory (can revise during execution)</option>
        <option value="confirmatory">
          Confirmatory (SAP-locked; deviations logged)
        </option>
      </select>
    </label>
  )
}

function CheckpointDropdown({
  value,
  mode,
  onChange,
  disabled,
}: {
  value: CheckpointChoice
  mode: SessionMode | undefined
  onChange: (v: CheckpointChoice) => void
  disabled?: boolean
}) {
  // Confirmatory + fast is rejected at the server. Disable Fast in
  // the UI when the current discipline is Confirmatory so the SME
  // can't pick an invalid combination.
  const isConfirmatory =
    typeof mode === 'object' && mode !== null && mode.kind === 'confirmatory'
  return (
    <label style={{ display: 'block' }}>
      <span
        style={{
          display: 'block',
          fontSize: '0.78rem',
          color: 'var(--color-text-secondary)',
          fontWeight: 500,
          marginBottom: '0.3rem',
        }}
      >
        Checkpoint discipline
      </span>
      <select
        value={value}
        onChange={(e) => onChange(e.target.value as CheckpointChoice)}
        disabled={disabled}
        style={{
          fontSize: '0.83rem',
          padding: '0.3rem 0.45rem',
          borderRadius: '0.35rem',
          border: '1px solid var(--color-border-strong)',
          background: 'var(--color-surface-1)',
          color: 'var(--color-text-primary)',
          minWidth: '18rem',
        }}
      >
        <option value="gated">Gated — review every stage (default)</option>
        <option value="selective">
          Selective — review only required stages
        </option>
        <option value="fast" disabled={isConfirmatory}>
          Fast — auto-advance every stage{isConfirmatory ? ' (disallowed in Confirmatory)' : ''}
        </option>
      </select>
    </label>
  )
}
