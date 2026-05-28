/**
 * `ClinicalConfirmGate` UI component.
 *
 * Renders a hard-stop confirmation prompt before a clinical /
 * regulated workflow can proceed. Lists every active policy bundle's
 * obligations (validated pipelines only, pinned containers, audit
 * trail, no generated code, etc.) and requires explicit click-through
 * with the SME's typed initials before the session can advance to
 * `start_execution`.
 *
 * Distinct from the existing `ConfirmationTurnCard` (which confirms
 * the LLM's understanding of the SME's intent); this gate confirms
 * the SME accepts the regulatory / safety obligations of running a
 * clinical workflow.
 */

import { useState } from 'react'

export interface ClinicalConfirmGateProps {
  bundle_id: string
  bundle_label: string
  bundle_description?: string
  obligations: string[]
  regulatory_citation?: string
  on_confirmed: (initials: string) => void
  on_canceled: () => void
}

export default function ClinicalConfirmGate({
  bundle_id,
  bundle_label,
  bundle_description,
  obligations,
  regulatory_citation,
  on_confirmed,
  on_canceled,
}: ClinicalConfirmGateProps) {
  const [initials, setInitials] = useState('')
  const [acknowledged, setAcknowledged] = useState(false)
  const canConfirm = acknowledged && initials.trim().length >= 2

  return (
    <div role="dialog" aria-label="Clinical confirmation gate" style={cardStyle}>
      <header style={headerStyle}>
        <h4 style={headingStyle}>{bundle_label}</h4>
        <code style={bundleIdStyle}>{bundle_id}</code>
      </header>
      {bundle_description && (
        <p style={descStyle}>{bundle_description}</p>
      )}
      <div style={obligationsBlock}>
        <strong style={obligationsHeading}>Obligations under this policy</strong>
        <ul style={obligationsList}>
          {obligations.map((o, i) => (
            <li key={i}>{prettyObligation(o)}</li>
          ))}
        </ul>
      </div>
      {regulatory_citation && (
        <p style={citationStyle}>Regulatory basis: {regulatory_citation}</p>
      )}
      <div style={ackBlock}>
        <label style={ackLabel}>
          <input
            type="checkbox"
            checked={acknowledged}
            onChange={(e) => setAcknowledged(e.target.checked)}
          />
          <span>
            I have reviewed every obligation listed above and accept
            responsibility for compliance under the named regulatory
            basis.
          </span>
        </label>
        <label style={initialsLabel}>
          Type your initials to confirm:
          <input
            type="text"
            value={initials}
            onChange={(e) => setInitials(e.target.value)}
            placeholder="e.g. AH"
            maxLength={6}
            style={initialsInput}
          />
        </label>
      </div>
      <div style={buttonRow}>
        <button onClick={on_canceled} style={cancelStyle}>
          Cancel
        </button>
        <button
          onClick={() => on_confirmed(initials.trim())}
          style={{
            ...confirmStyle,
            opacity: canConfirm ? 1 : 0.5,
            cursor: canConfirm ? 'pointer' : 'not-allowed',
          }}
          disabled={!canConfirm}
        >
          Confirm and proceed
        </button>
      </div>
    </div>
  )
}

function prettyObligation(o: string): string {
  switch (o) {
    case 'validated_nodes_only':
      return 'Every node must be lifecycle-validated (Production / Benchmark / Locally validated)'
    case 'require_pinned_containers':
      return 'Every container must carry a pinned digest'
    case 'no_generated_code':
      return 'No generated-code implementations'
    case 'no_policy_restricted_adapters':
      return 'No policy-restricted adapters (cross-reference variant normalization, etc.)'
    case 'no_network':
      return 'No network access in any executor task'
    case 'pinned_reference_data_only':
      return 'Reference data must be pinned (assembly with patch number, annotation with release)'
    case 'human_signoff_required':
      return 'Human signoff required before executable status'
    case 'audit_trail_required':
      return 'Full audit trail required (decisions.jsonl populated for every non-default decision)'
    case 'no_privacy_widening':
      return 'No edges that widen privacy class (PHI → public, etc.)'
    default:
      return o.replace(/_/g, ' ')
  }
}

const cardStyle: React.CSSProperties = {
  padding: '1rem 1.2rem',
  fontSize: '0.82rem',
  background: 'var(--color-surface-1)',
  border: '2px solid #7a1212',
  borderRadius: '0.4rem',
  maxWidth: '600px',
}
const headerStyle: React.CSSProperties = {
  display: 'flex',
  alignItems: 'baseline',
  justifyContent: 'space-between',
  marginBottom: '0.4rem',
}
const headingStyle: React.CSSProperties = {
  margin: 0,
  fontSize: '1rem',
  color: 'var(--color-danger-fg)',
}
const bundleIdStyle: React.CSSProperties = {
  fontFamily: 'ui-monospace, monospace',
  fontSize: '0.7rem',
  color: 'var(--color-text-muted)',
}
const descStyle: React.CSSProperties = {
  margin: '0.2rem 0 0.5rem',
  fontSize: '0.78rem',
  color: 'var(--color-text-secondary)',
}
const obligationsBlock: React.CSSProperties = {
  marginTop: '0.6rem',
}
const obligationsHeading: React.CSSProperties = {
  display: 'block',
  fontSize: '0.78rem',
  marginBottom: '0.3rem',
}
const obligationsList: React.CSSProperties = {
  margin: 0,
  paddingLeft: '1.1rem',
  fontSize: '0.74rem',
  color: 'var(--color-text-primary)',
}
const citationStyle: React.CSSProperties = {
  margin: '0.5rem 0 0',
  fontSize: '0.72rem',
  color: 'var(--color-text-muted)',
  fontStyle: 'italic',
}
const ackBlock: React.CSSProperties = {
  marginTop: '0.7rem',
  padding: '0.5rem',
  background: 'var(--color-surface-muted)',
  borderRadius: '0.3rem',
}
const ackLabel: React.CSSProperties = {
  display: 'flex',
  gap: '0.5rem',
  alignItems: 'flex-start',
  fontSize: '0.78rem',
  cursor: 'pointer',
}
const initialsLabel: React.CSSProperties = {
  display: 'flex',
  gap: '0.4rem',
  alignItems: 'center',
  marginTop: '0.5rem',
  fontSize: '0.78rem',
}
const initialsInput: React.CSSProperties = {
  padding: '0.25rem 0.5rem',
  width: '6rem',
  fontSize: '0.78rem',
  border: '1px solid var(--color-border-subtle)',
  borderRadius: '0.3rem',
  fontFamily: 'ui-monospace, monospace',
}
const buttonRow: React.CSSProperties = {
  display: 'flex',
  gap: '0.5rem',
  justifyContent: 'flex-end',
  marginTop: '0.7rem',
}
const cancelStyle: React.CSSProperties = {
  padding: '0.4rem 1rem',
  fontSize: '0.78rem',
  background: 'var(--color-surface-1)',
  color: 'var(--color-text-primary)',
  border: '1px solid var(--color-border-subtle)',
  borderRadius: '0.3rem',
  cursor: 'pointer',
}
const confirmStyle: React.CSSProperties = {
  padding: '0.4rem 1rem',
  fontSize: '0.78rem',
  background: 'var(--color-danger-fg)',
  color: '#fff',
  border: 'none',
  borderRadius: '0.3rem',
}
