// Inline turn-card for literature-grounded `discover_*` method-choice
// stages. Rendered when the session surfaces a
// BlockerKind::AwaitingSmeSelection whose `stage_id` begins with
// `discover_`. Shows a radio group of ranked candidate methods — each
// annotated with its deterministic composite score, literature
// eligibility, an optional "tentative" (needs-promotion) flag, and a
// list of locator-anchored evidence quotes — plus an optional rationale
// textarea. Rank-1 is marked "★ Recommended" and pre-selected so the
// zero-touch default is one click. On submit, dispatches the chosen
// method (and optional rationale) through `onSelect`.
//
// This is a presentational component: it owns local
// `MethodOption`/`MethodEvidence` interfaces rather than a generated
// ts-rs type, so callers map whatever shape they hold into these.

import { useState } from 'react'
import type {
  MethodLandscape,
  MethodLandscapeCandidate,
} from '../api/chatClient'

export interface MethodEvidence {
  sourceClass: string
  ref: string
  quote: string
  versionContext?: string
}

export interface MethodOption {
  method: string
  score: number
  literatureEligible: boolean
  /** Not in the curated pool; requires promotion before it can run. */
  tentative?: boolean
  evidence: MethodEvidence[]
}

/// Strip the `discover_` prefix from a blocker `stage_id` to get the
/// method-choice axis key the survey artifact is keyed by
/// (e.g. `discover_alignment` -> `alignment`). A non-`discover_` id is
/// returned unchanged so the helper is safe to call defensively.
export function axisFromStageId(stageId: string): string {
  return stageId.startsWith('discover_')
    ? stageId.slice('discover_'.length)
    : stageId
}

/// Build a single MethodEvidence row from a landscape candidate's
/// evidence entry. Null locator parts collapse to a readable ref.
function toMethodEvidence(
  e: MethodLandscapeCandidate['evidence'][number],
): MethodEvidence {
  const kind = e.source_ref_kind?.trim()
  const ref = e.source_ref?.trim()
  // Prefer `KIND:ref` (e.g. `pmid:30000000`) when both present; otherwise
  // whichever locator part exists; otherwise the source class so the row
  // is never blank.
  const display =
    ref && kind ? `${kind}:${ref}` : ref || kind || e.source_class
  return {
    sourceClass: e.source_class,
    ref: display,
    quote: e.evidence_quote,
    ...(e.version_context ? { versionContext: e.version_context } : {}),
  }
}

/// Map a `survey_method_landscape` artifact into the ranked
/// `MethodOption[]` this card renders, for one method-choice `axis`
/// (the `discover_`-stripped blocker stage id). Returns `null` when the
/// landscape is missing or the axis is absent so the caller can fall
/// back to bare candidate names. Candidates are sorted by descending
/// `support_score` (stable on ties by method name) so rank-1 — the
/// "★ Recommended" zero-touch default — is the highest-scored method.
export function mapLandscapeToOptions(
  landscape: MethodLandscape | null | undefined,
  axis: string,
): MethodOption[] | null {
  const candidates = landscape?.axes?.[axis]?.candidates
  if (!candidates || candidates.length === 0) return null
  return candidates
    .map((c) => ({
      method: c.method,
      score: c.support_score,
      literatureEligible: c.literature_eligible,
      tentative: c.tentative,
      evidence: (c.evidence ?? []).map(toMethodEvidence),
    }))
    .sort((a, b) =>
      b.score !== a.score ? b.score - a.score : a.method.localeCompare(b.method),
    )
}

interface Props {
  stage: string
  options: MethodOption[]
  disabled?: boolean
  onSelect: (method: string, rationale?: string) => void | Promise<void>
}

export function MethodOptionsCard({
  stage,
  options,
  disabled,
  onSelect,
}: Props) {
  const [chosen, setChosen] = useState<string>(options[0]?.method ?? '')
  const [rationale, setRationale] = useState('')
  const canSubmit = !disabled && chosen !== ''

  // Sanitize stage to a valid HTML id fragment; stage ids are
  // server-supplied so could carry whitespace or special chars.
  const groupName = `method-${stage.replace(/[^a-zA-Z0-9_-]/g, '_')}`
  const headingId = `${groupName}-heading`

  return (
    <section
      role="region"
      aria-labelledby={headingId}
      data-stage-id={stage}
      style={{
        marginTop: '0.75rem',
        padding: '0.85rem 1rem',
        background: 'var(--color-info-bg)',
        border: '1px solid #93c5fd',
        borderLeft: '4px solid #2563eb',
        borderRadius: 8,
      }}
    >
      <h3
        id={headingId}
        style={{
          margin: 0,
          marginBottom: 4,
          fontSize: '0.9rem',
          color: 'var(--color-info-fg)',
        }}
      >
        Method options for <span title={stage}>{stage}</span>
      </h3>
      <p
        style={{
          margin: '0 0 0.5rem',
          fontSize: '0.78rem',
          color: 'var(--color-text-muted)',
        }}
      >
        Ranked from retrieved literature. Pick one or keep the recommended
        default.
      </p>

      {options.length === 0 ? (
        <p
          style={{
            margin: 0,
            fontSize: '0.82rem',
            color: 'var(--color-info-fg)',
            fontStyle: 'italic',
          }}
        >
          No candidates yet — the method landscape hasn't arrived.
        </p>
      ) : (
        options.map((o, i) => {
          const selected = chosen === o.method
          return (
            <label
              key={o.method}
              style={{
                display: 'block',
                marginTop: '0.5rem',
                border: selected ? '2px solid #2563eb' : '1px solid #bfdbfe',
                borderRadius: 6,
                padding: '0.5rem 0.6rem',
                background: 'var(--color-surface-1)',
              }}
            >
              <input
                type="radio"
                name={groupName}
                aria-label={o.method}
                checked={selected}
                disabled={disabled}
                onChange={() => setChosen(o.method)}
              />
              <strong style={{ marginLeft: '0.3rem' }}>{o.method}</strong>
              <span
                style={{ marginLeft: '0.4rem', color: 'var(--color-text-muted)' }}
              >
                score {o.score.toFixed(2)}
              </span>
              {i === 0 && (
                <span
                  style={{
                    marginLeft: '0.4rem',
                    fontWeight: 600,
                    color: 'var(--color-success-accent)',
                  }}
                >
                  ★ Recommended
                </span>
              )}
              {o.tentative && (
                <span
                  title="not in curated pool; needs promotion before it can run"
                  style={{
                    marginLeft: '0.4rem',
                    color: 'var(--color-warning-accent)',
                  }}
                >
                  · tentative
                </span>
              )}
              {!o.literatureEligible && (
                <span
                  style={{
                    marginLeft: '0.4rem',
                    color: 'var(--color-text-muted)',
                  }}
                >
                  · no paper-class evidence
                </span>
              )}
              {o.evidence.length > 0 && (
                <ul
                  style={{
                    margin: '0.25rem 0 0 1rem',
                    padding: 0,
                    fontSize: '0.76rem',
                    color: 'var(--color-text-secondary)',
                  }}
                >
                  {o.evidence.map((e, j) => (
                    <li key={j}>
                      [{e.sourceClass}] {e.ref}
                      {e.versionContext ? ` (v ${e.versionContext})` : ''}: “
                      {e.quote}”
                    </li>
                  ))}
                </ul>
              )}
            </label>
          )
        })
      )}

      <textarea
        rows={2}
        value={rationale}
        onChange={(e) => setRationale(e.target.value)}
        disabled={disabled}
        aria-label="Optional rationale"
        placeholder="Why this choice? (optional)"
        style={{
          width: '100%',
          boxSizing: 'border-box',
          marginTop: '0.5rem',
          padding: '0.4rem 0.55rem',
          fontSize: '0.8rem',
          fontFamily: 'inherit',
          border: '1px solid #bfdbfe',
          borderRadius: 6,
          resize: 'vertical',
        }}
      />

      <button
        type="button"
        disabled={!canSubmit}
        onClick={() => onSelect(chosen, rationale.trim() ? rationale.trim() : undefined)}
        style={{
          marginTop: '0.5rem',
          padding: '0.45rem 0.9rem',
          background: canSubmit
            ? 'var(--color-accent)'
            : 'var(--color-info-border)',
          color: 'var(--color-text-on-accent)',
          border: 'none',
          borderRadius: 6,
          cursor: canSubmit ? 'pointer' : 'not-allowed',
          fontSize: '0.8rem',
          fontWeight: 600,
        }}
      >
        Record choice
      </button>
    </section>
  )
}

export default MethodOptionsCard
