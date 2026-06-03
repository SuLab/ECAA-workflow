import type { CoverageConfidence } from '../types/CoverageConfidence'

interface Props {
  coverage: CoverageConfidence
  onProposeDraft: () => void
  disabled?: boolean
}

/**
 * Communicates catalog-coverage uncertainty before the confirmation gate.
 * Renders nothing when the composition is fully covered. When some
 * requested modality fell outside the validated catalog, it surfaces an
 * SME-legible message + the uncovered modalities + a CTA that the parent
 * wires to the EXISTING `propose_hypothesized_node` tool (no new endpoint).
 */
export default function CoverageConfidenceCard({ coverage, onProposeDraft, disabled }: Props) {
  if (coverage.fully_covered) return null

  const uncovered = coverage.uncovered_modalities
  const hasUncovered = uncovered.length > 0

  return (
    <div
      role="status"
      aria-label="Catalog coverage confidence"
      style={{
        marginTop: '0.6rem',
        padding: '0.7rem 0.9rem',
        background: 'var(--color-surface-1)',
        border: '1px solid var(--color-border-strong)',
        borderRadius: 8,
        fontSize: '0.85rem',
        color: 'var(--color-text-primary)',
      }}
    >
      <p style={{ margin: '0 0 0.4rem' }}>
        {hasUncovered
          ? 'Part of your request is outside our validated catalog. I can proceed by drafting a candidate step for it — you stay in control of whether it runs.'
          : 'This plan has some unresolved coverage gaps. I can proceed by drafting candidate steps for them.'}
      </p>
      {hasUncovered && (
        <ul
          aria-label="Modalities outside the validated catalog"
          style={{ margin: '0 0 0.5rem', paddingLeft: '1.1rem' }}
        >
          {uncovered.map((m) => (
            <li key={m}>{m}</li>
          ))}
        </ul>
      )}
      <button
        type="button"
        onClick={onProposeDraft}
        disabled={disabled}
        style={{
          padding: '0.35rem 0.8rem',
          background: 'var(--color-surface-2)',
          border: '1px solid var(--color-border-strong)',
          borderRadius: 6,
          cursor: disabled ? 'not-allowed' : 'pointer',
          fontSize: '0.8rem',
          color: 'var(--color-text-primary)',
          opacity: disabled ? 0.5 : 1,
        }}
      >
        Draft a candidate step
      </button>
    </div>
  )
}
