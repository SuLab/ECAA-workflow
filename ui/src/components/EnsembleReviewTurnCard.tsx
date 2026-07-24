// In-chat summary card for a completed multi-analyst ensemble run.
// Fetches `getEnsembleDistribution` on mount and renders nothing
// (`return null`) when the session never ran an ensemble — that's the
// dominant case for ordinary (non-ensemble) sessions, so this stays
// silent rather than showing an empty/error card. When an ensemble is
// present, surfaces the headline numbers inline and links out to the
// full per-cell / per-entity breakdown on the Robustness tab
// (`EnsembleTab`, the `'ensemble'` id in `StateInspectorPane`) rather
// than duplicating that detail here.

import { useState } from 'react'
import { getEnsembleDistribution } from '../api/chatClient'
import { useCancelableEffect } from '../hooks/useCancelableFetch'
import type { EnsembleDistribution } from '../types/EnsembleDistribution'
import { CardContainer } from './primitives/CardContainer'

interface Props {
  sessionId: string
  /** Switches `StateInspectorPane`'s controlled tab to `'ensemble'`
   *  (the Robustness tab) — see `App.tsx`'s `handleOpenRobustnessTab`. */
  onOpenRobustnessTab: () => void
}

export default function EnsembleReviewTurnCard({
  sessionId,
  onOpenRobustnessTab,
}: Props): JSX.Element | null {
  const [ensemble, setEnsemble] = useState<EnsembleDistribution | null>(null)

  useCancelableEffect(async ({ cancelled }) => {
    try {
      const result = await getEnsembleDistribution(sessionId)
      if (!cancelled()) setEnsemble(result)
    } catch {
      // Non-fatal — an ensemble rollup is opt-in; treat a fetch failure
      // the same as "no ensemble ran" rather than surfacing an error
      // card for an optional feature.
      if (!cancelled()) setEnsemble(null)
    }
  }, [sessionId])

  if (!ensemble) return null

  const cellCount = ensemble.cells.length
  const nPruned = Number(ensemble.n_pruned)
  const agreementPct = Math.round(ensemble.agreement * 100)

  return (
    <CardContainer
      palette="info"
      role="region"
      ariaLabel="Ensemble review"
      dataAttrs={{ 'data-ensemble-review': 'true' }}
    >
      <header style={{ marginBottom: 6 }}>
        <strong style={{ fontSize: '0.85rem', color: 'var(--color-info-fg)' }}>
          Ensemble review
        </strong>
      </header>
      <p
        style={{
          margin: 0,
          fontSize: '0.81rem',
          color: 'var(--color-info-fg)',
          lineHeight: 1.4,
        }}
      >
        {cellCount} analyst cell{cellCount === 1 ? '' : 's'} ran; {ensemble.consensus_label};
        agreement {agreementPct}%; {nPruned} pruned/flagged.
      </p>
      <p
        style={{
          margin: '0.4rem 0 0',
          fontSize: '0.72rem',
          fontStyle: 'italic',
          color: 'var(--color-text-secondary)',
        }}
      >
        Robustness, not verified truth.
      </p>
      <div style={{ marginTop: '0.6rem' }}>
        <button
          type="button"
          onClick={onOpenRobustnessTab}
          style={{
            padding: '0.4rem 0.85rem',
            background: 'transparent',
            color: 'var(--color-info-fg)',
            border: '1px solid var(--color-info-border)',
            borderRadius: 6,
            cursor: 'pointer',
            fontSize: '0.78rem',
            fontWeight: 600,
          }}
        >
          View robustness details
        </button>
      </div>
    </CardContainer>
  )
}
