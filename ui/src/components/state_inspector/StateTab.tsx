import { useState } from 'react'
import { useCancelableEffect } from '../../hooks/useCancelableFetch'

import { getComposeOutcome, type SessionStateSnapshot } from '../../api/chatClient'
import PopulationCoverageCard from './PopulationCoverageCard'

/**
 * Status tab body. The Tab id is `state` for legacy routing reasons —
 * the label exposed to SMEs is "Status". Surfaces:
 *
 *  - v3 P9 `PopulationCoverageCard` — pulled in unconditionally; the
 *  card self-renders an empty placeholder when the active workflow
 *  has no coverage YAML on file. When the most-recent compose
 *  outcome was a `PopulationOutOfCoverage` refusal, the card flips
 *  to its banner mode and exposes the waiver CTA.
 *  - The raw `SessionStateSnapshot` JSON underneath — preserved for
 *  debug / inspection.
 */
export function StateTab({
  state,
  sessionId,
}: {
  state: SessionStateSnapshot | null
  /**
   * Session id — when provided, drives the PopulationCoverageCard's
   * fetch + waiver-recording paths. Optional so existing callers that
   * pass only `state` (legacy tests) keep compiling.
   */
  sessionId?: string | null
}): JSX.Element {
  const [populationRefusal, setPopulationRefusal] = useState<{
    workflow_id: string
    sample_label: string
    validated_labels: string[]
    suggested_waiver_authority: string
    policy_rule_id: string
  } | null>(null)

  // v3 P9 — when the compose outcome is a `PopulationOutOfCoverage`
  // refusal, flip the card into banner mode. The card otherwise stands
  // on its own (it fetches the coverage statement for the active
  // workflow on mount).
  useCancelableEffect(async ({ cancelled }) => {
    if (!sessionId) {
      setPopulationRefusal(null)
      return
    }
    try {
      const outcome = await getComposeOutcome(sessionId)
      if (cancelled()) return
      if (
        outcome &&
        outcome.variant === 'refusal' &&
        outcome.refusal &&
        isPopulationOutOfCoverage(outcome.refusal)
      ) {
        const k = (outcome.refusal as Record<string, unknown>).kind as Record<string, unknown>
        setPopulationRefusal({
          workflow_id: String(k.workflow_id ?? ''),
          sample_label: String(k.sample_label ?? ''),
          validated_labels: Array.isArray(k.validated_labels)
            ? (k.validated_labels as unknown[]).map(String)
            : [],
          suggested_waiver_authority: String(
            k.suggested_waiver_authority ?? 'clinical_lead',
          ),
          // The server's refusal report doesn't directly carry the
          // policy_rule_id on PopulationOutOfCoverage — the waiver
          // endpoint takes the rule id from the active policy bundle.
          // Fall back to a canonical placeholder; the server resolves
          // the real id at acceptance time.
          policy_rule_id: 'population_coverage',
        })
      } else {
        setPopulationRefusal(null)
      }
    } catch {
      if (!cancelled()) setPopulationRefusal(null)
    }
  }, [sessionId])

  return (
    <div style={containerStyle} aria-label="Session status">
      {sessionId && (
        <div style={cardWrapStyle}>
          <PopulationCoverageCard
            sessionId={sessionId}
            refusal={populationRefusal ?? undefined}
          />
        </div>
      )}
      <pre style={preStyle}>
        {state ? JSON.stringify(state, null, 2) : 'Loading…'}
      </pre>
    </div>
  )
}

/**
 * Runtime check for `RefusalKind::PopulationOutOfCoverage`. The server
 * serializes the typed kind with a `kind` discriminator one level deep
 * (`refusal.kind.kind === "population_out_of_coverage"`); this helper
 * keeps the effect callsite readable.
 */
function isPopulationOutOfCoverage(raw: unknown): boolean {
  if (!raw || typeof raw !== 'object') return false
  const r = raw as Record<string, unknown>
  const kind = r.kind
  if (!kind || typeof kind !== 'object') return false
  return (kind as Record<string, unknown>).kind === 'population_out_of_coverage'
}

const containerStyle: React.CSSProperties = {
  padding: '1rem',
  display: 'flex',
  flexDirection: 'column',
  gap: '1rem',
  overflow: 'auto',
  height: '100%',
}

const cardWrapStyle: React.CSSProperties = {
  flexShrink: 0,
}

const preStyle: React.CSSProperties = {
  margin: 0,
  fontSize: '0.82rem',
  color: 'var(--color-text-primary)',
  fontFamily: 'monospace',
  whiteSpace: 'pre',
  overflow: 'auto',
}
