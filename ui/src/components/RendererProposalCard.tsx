// Modal-inline form for the SME to describe a preferred renderer when a
// figure resolved via StructuralFallback. Bound to
// `proposeHypothesizedRenderer` in the chat client.
//
// The form is visually consistent with BlockerCard (same padding / border /
// border-radius / background as the BlockerCard warning surface at line 672).

import { useState } from 'react'
import { proposeHypothesizedRenderer } from '../api/chatClient'

interface Props {
  sessionId: string
  targetSemanticType: string
  /**
   * Generic primitive that resolved for this figure (e.g. `matrix_overview`).
   * Prepopulates the `primitive_basis` field and is shown as a hint.
   * null when the figure has no structural fallback.
   */
  primitiveBasis: string | null
  /**
   * Registered parent-term IRIs exposed to the SME as selectable
   * inheritance targets. Populated from the affordance proof's
   * ontology_walk when available. An empty list suppresses the select
   * and the proposal requires at least one term typed in free-form (Phase
   * 9 simplified: always show a select; empty list shows "(none)" only).
   */
  availableParentTerms: string[]
  onAccepted(proposalId: string): void
  onCancel(): void
}

const CARD_STYLE: React.CSSProperties = {
  padding: '0.85rem 1rem',
  background: 'var(--color-surface-0)',
  border: '1px solid var(--color-border-default)',
  borderRadius: 8,
  display: 'flex',
  flexDirection: 'column',
  gap: '0.65rem',
}

const LABEL_STYLE: React.CSSProperties = {
  display: 'flex',
  flexDirection: 'column',
  gap: '0.3rem',
  fontSize: '0.83rem',
  color: 'var(--color-text-secondary)',
  fontWeight: 500,
}

const INPUT_STYLE: React.CSSProperties = {
  width: '100%',
  padding: '0.4rem 0.5rem',
  fontSize: '0.83rem',
  border: '1px solid var(--color-border-default)',
  borderRadius: 4,
  background: 'var(--color-surface-1)',
  color: 'var(--color-text-primary)',
  boxSizing: 'border-box',
}

const TEXTAREA_STYLE: React.CSSProperties = {
  ...INPUT_STYLE,
  resize: 'vertical',
  fontFamily: 'inherit',
  lineHeight: 1.45,
}

export function RendererProposalCard({
  sessionId,
  targetSemanticType,
  primitiveBasis,
  availableParentTerms,
  onAccepted,
  onCancel,
}: Props): JSX.Element {
  const [smeIntent, setSmeIntent] = useState('')
  const [parentTerm, setParentTerm] = useState(
    availableParentTerms[0] ?? '',
  )
  // Figure ids: one per non-blank line in the textarea.
  const [figureIdsText, setFigureIdsText] = useState('')
  const [submitting, setSubmitting] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const canSubmit = smeIntent.trim().length > 0 && !submitting

  async function submit() {
    if (!canSubmit) return
    setSubmitting(true)
    setError(null)
    try {
      const result = await proposeHypothesizedRenderer({
        sessionId,
        targetSemanticType,
        proposedParentTerms: parentTerm ? [parentTerm] : [],
        proposedFigureIds: figureIdsText
          .split('\n')
          .map((s) => s.trim())
          .filter(Boolean),
        smeIntent: smeIntent.trim(),
        primitiveBasis,
      })
      if (result.outcome === 'proposal_accepted') {
        onAccepted(result.proposal_id)
      } else {
        setError(result.reason)
      }
    } catch (err) {
      setError(String(err))
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <div role="dialog" aria-label="Describe a preferred plot" style={CARD_STYLE}>
      <h3
        style={{
          margin: 0,
          fontSize: '0.92rem',
          fontWeight: 600,
          color: 'var(--color-text-primary)',
        }}
      >
        Describe a preferred plot
      </h3>
      {primitiveBasis && (
        <p
          style={{
            margin: 0,
            fontSize: '0.78rem',
            color: 'var(--color-text-secondary)',
          }}
        >
          Currently using a generic <strong>{primitiveBasis}</strong> plot for{' '}
          <code style={{ fontFamily: 'ui-monospace, monospace', fontSize: '0.75rem' }}>
            {targetSemanticType}
          </code>
          .
        </p>
      )}
      <p style={{ margin: 0, fontSize: '0.83rem', color: 'var(--color-text-secondary)' }}>
        What kind of plot would you like for this result?
      </p>
      <label style={LABEL_STYLE}>
        Your description
        <textarea
          value={smeIntent}
          onChange={(e) => setSmeIntent(e.target.value)}
          rows={4}
          aria-label="SME description of preferred plot"
          placeholder="e.g. 'a violin plot grouped by treatment with significance markers'"
          disabled={submitting}
          style={TEXTAREA_STYLE}
        />
      </label>
      {availableParentTerms.length > 0 && (
        <label style={LABEL_STYLE}>
          Inherit renderer from
          <select
            value={parentTerm}
            onChange={(e) => setParentTerm(e.target.value)}
            aria-label="Parent term for renderer inheritance"
            disabled={submitting}
            style={INPUT_STYLE}
          >
            <option value="">(none — propose new renderer from scratch)</option>
            {availableParentTerms.map((t) => (
              <option key={t} value={t}>
                {t}
              </option>
            ))}
          </select>
        </label>
      )}
      <label style={LABEL_STYLE}>
        Proposed figure ids{' '}
        <span style={{ fontWeight: 400, fontSize: '0.75rem' }}>(one per line, optional)</span>
        <textarea
          value={figureIdsText}
          onChange={(e) => setFigureIdsText(e.target.value)}
          rows={3}
          aria-label="Proposed figure ids, one per line"
          placeholder="e.g.&#10;volcano&#10;ridge_plot"
          disabled={submitting}
          style={TEXTAREA_STYLE}
        />
      </label>
      {error && (
        <div
          role="alert"
          style={{
            padding: '0.5rem 0.65rem',
            background: 'var(--color-danger-bg)',
            border: '1px solid var(--color-danger-border)',
            borderRadius: 4,
            fontSize: '0.83rem',
            color: 'var(--color-danger-fg)',
          }}
        >
          {error}
        </div>
      )}
      <div style={{ display: 'flex', gap: '0.5rem', justifyContent: 'flex-end' }}>
        <button
          type="button"
          onClick={onCancel}
          disabled={submitting}
          style={{
            padding: '0.4rem 0.85rem',
            fontSize: '0.83rem',
            background: 'transparent',
            color: 'var(--color-text-secondary)',
            border: '1px solid var(--color-border-default)',
            borderRadius: 6,
            cursor: submitting ? 'not-allowed' : 'pointer',
          }}
        >
          Cancel
        </button>
        <button
          type="button"
          onClick={submit}
          disabled={!canSubmit}
          aria-disabled={!canSubmit}
          style={{
            padding: '0.4rem 0.85rem',
            fontSize: '0.83rem',
            fontWeight: 600,
            background: canSubmit
              ? 'var(--color-button-primary-bg)'
              : 'var(--color-border-strong)',
            color: canSubmit
              ? 'var(--color-button-primary-fg)'
              : 'var(--color-text-muted)',
            border: 'none',
            borderRadius: 6,
            cursor: canSubmit ? 'pointer' : 'not-allowed',
          }}
        >
          {submitting ? 'Proposing…' : 'Propose'}
        </button>
      </div>
    </div>
  )
}
