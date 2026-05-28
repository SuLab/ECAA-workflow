/**
 * v3 P9 §11.X — `PopulationCoverageCard`.
 *
 * Renders the workflow's validated cohort set for the active session and
 * surfaces a "Request waiver" affordance when the composer's
 * `RefusalKind::PopulationOutOfCoverage` refusal is in scope.
 *
 * Framing constraint (v3 §11.X): the card describes the *workflow's*
 * validation envelope, not the user's identity or access. The waiver
 * dialog is a structured CTA to record that the workflow's envelope is
 * being explicitly exceeded with a named authority's sign-off.
 */

import React, { useState } from 'react'
import { jsonFetch, voidFetch } from '../../api/_fetch'
import { useCancelableEffect } from '../../hooks/useCancelableFetch'

interface CohortDescriptor {
  label: string
  population_code?: string
  age_band?: string
  sample_type?: string
  n?: number
}

interface PopulationCoverageStatement {
  workflow_id: string
  validated_cohorts: CohortDescriptor[]
  explicitly_untested?: CohortDescriptor[]
  citations?: string[]
}

interface PopulationCoverageResponse {
  workflow_id: string | null
  statement: PopulationCoverageStatement | null
}

interface RefusalInfo {
  workflow_id: string
  sample_label: string
  validated_labels: string[]
  suggested_waiver_authority: string
  policy_rule_id: string
}

interface Props {
  sessionId: string
  /** When present, renders the out-of-coverage banner + waiver CTA. */
  refusal?: RefusalInfo
  /** Fires after a waiver POST succeeds so the parent can refresh state. */
  onWaiverRecorded?: () => void
}

export default function PopulationCoverageCard({
  sessionId,
  refusal,
  onWaiverRecorded,
}: Props): JSX.Element {
  const [resp, setResp] = useState<PopulationCoverageResponse | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [showWaiverForm, setShowWaiverForm] = useState(false)
  const [waiverRationale, setWaiverRationale] = useState('')
  const [waiverAuthority, setWaiverAuthority] = useState(
    refusal?.suggested_waiver_authority ?? 'clinical_lead'
  )
  const [submitting, setSubmitting] = useState(false)
  const [submitError, setSubmitError] = useState<string | null>(null)

  useCancelableEffect(async ({ signal, cancelled }) => {
    setLoadError(null)
    try {
      const data = await jsonFetch<PopulationCoverageResponse>(
        `/api/chat/session/${encodeURIComponent(sessionId)}/population-coverage`,
        { signal },
      )
      if (!cancelled()) setResp(data)
    } catch (e: unknown) {
      if (!cancelled()) setLoadError(String(e))
    }
  }, [sessionId])

  async function submitWaiver() {
    if (!refusal) return
    if (!waiverRationale.trim()) {
      setSubmitError('Rationale is required.')
      return
    }
    setSubmitting(true)
    setSubmitError(null)
    try {
      await voidFetch(
        `/api/chat/session/${encodeURIComponent(sessionId)}/population-waiver`,
        {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            workflow_id: refusal.workflow_id,
            waiving_authority: waiverAuthority,
            rationale: waiverRationale,
            policy_rule_id: refusal.policy_rule_id,
          }),
        }
      )
      setSubmitting(false)
      setShowWaiverForm(false)
      setWaiverRationale('')
      if (onWaiverRecorded) onWaiverRecorded()
    } catch (e: unknown) {
      setSubmitError(String(e))
      setSubmitting(false)
    }
  }

  return (
    <section
      role="region"
      aria-label="Population coverage"
      style={sectionStyle}
    >
      <h3 style={headingStyle}>Population coverage</h3>

      {loadError && (
        <p style={errorStyle}>Failed to load coverage statement: {loadError}</p>
      )}

      {!loadError && !resp && <p style={mutedStyle}>Loading…</p>}

      {resp && resp.statement && (
        <>
          <p style={bodyStyle}>
            Workflow <code style={codeStyle}>{resp.workflow_id}</code> has
            been validated on the following cohorts:
          </p>
          <ul style={listStyle}>
            {resp.statement.validated_cohorts.map((c) => (
              <li key={c.label}>
                <strong>{c.label}</strong>
                {(c.age_band || c.sample_type) && (
                  <span style={mutedSpanStyle}>
                    {' '}
                    ({[c.age_band, c.sample_type].filter(Boolean).join(', ')}
                    {c.n ? `, n=${c.n}` : ''})
                  </span>
                )}
              </li>
            ))}
          </ul>

          {resp.statement.explicitly_untested &&
            resp.statement.explicitly_untested.length > 0 && (
              <>
                <p style={bodyStyle}>
                  <strong>Not validated on:</strong>
                </p>
                <ul style={listStyle}>
                  {resp.statement.explicitly_untested.map((c) => (
                    <li key={c.label}>
                      <em>{c.label}</em>
                      {(c.age_band || c.sample_type) && (
                        <span style={mutedSpanStyle}>
                          {' '}
                          (
                          {[c.age_band, c.sample_type]
                            .filter(Boolean)
                            .join(', ')}
                          )
                        </span>
                      )}
                    </li>
                  ))}
                </ul>
              </>
            )}

          {resp.statement.citations && resp.statement.citations.length > 0 && (
            <>
              <p style={mutedStyle}>References:</p>
              <ul style={citationListStyle}>
                {resp.statement.citations.map((c) => (
                  <li key={c}>
                    <code style={codeStyle}>{c}</code>
                  </li>
                ))}
              </ul>
            </>
          )}
        </>
      )}

      {resp && !resp.statement && resp.workflow_id && (
        <p style={mutedStyle}>
          No coverage statement on file for workflow{' '}
          <code style={codeStyle}>{resp.workflow_id}</code>. The composer's
          population-coverage gate does not apply to this workflow.
        </p>
      )}

      {resp && !resp.workflow_id && (
        <p style={mutedStyle}>
          No workflow archetype recorded for this session yet; the
          coverage gate will activate once the composer selects an
          archetype.
        </p>
      )}

      {refusal && (
        <div
          role="alert"
          aria-label="Sample cohort outside coverage"
          style={refusalBoxStyle}
        >
          <p style={refusalHeadingStyle}>
            Sample cohort <strong>{refusal.sample_label}</strong> is outside
            the validated set.
          </p>
          <p style={refusalBodyStyle}>
            The composer refused to proceed because workflow{' '}
            <code style={codeStyle}>{refusal.workflow_id}</code> has not
            been validated on this cohort.
          </p>
          {!showWaiverForm && (
            <button
              type="button"
              onClick={() => setShowWaiverForm(true)}
              style={primaryButtonStyle}
            >
              Request waiver from {refusal.suggested_waiver_authority}
            </button>
          )}
          {showWaiverForm && (
            <div style={waiverFormStyle}>
              <label style={labelStyle}>
                Waiving authority:
                <input
                  type="text"
                  value={waiverAuthority}
                  onChange={(e) => setWaiverAuthority(e.target.value)}
                  style={inputStyle}
                />
              </label>
              <label style={labelStyle}>
                Rationale (required):
                <textarea
                  value={waiverRationale}
                  onChange={(e) => setWaiverRationale(e.target.value)}
                  rows={3}
                  style={textareaStyle}
                  placeholder="Why is processing this out-of-coverage cohort acceptable?"
                />
              </label>
              {submitError && <p style={errorStyle}>{submitError}</p>}
              <div style={buttonRowStyle}>
                <button
                  type="button"
                  onClick={() => {
                    setShowWaiverForm(false)
                    setWaiverRationale('')
                    setSubmitError(null)
                  }}
                  style={secondaryButtonStyle}
                  disabled={submitting}
                >
                  Cancel
                </button>
                <button
                  type="button"
                  onClick={submitWaiver}
                  style={primaryButtonStyle}
                  disabled={submitting || !waiverRationale.trim()}
                >
                  {submitting ? 'Recording…' : 'Record waiver'}
                </button>
              </div>
            </div>
          )}
        </div>
      )}
    </section>
  )
}

const sectionStyle: React.CSSProperties = {
  padding: '0.75rem 1rem',
  fontSize: '0.82rem',
  background: 'var(--color-surface-muted, #f6f6f6)',
  border: '1px solid var(--color-border-subtle, #ccc)',
  borderRadius: '0.4rem',
  lineHeight: 1.45,
}
const headingStyle: React.CSSProperties = {
  margin: '0 0 0.5rem',
  fontSize: '0.95rem',
  fontWeight: 600,
}
const bodyStyle: React.CSSProperties = {
  margin: '0.3rem 0',
}
const listStyle: React.CSSProperties = {
  margin: '0.3rem 0 0.5rem',
  paddingLeft: '1.2rem',
}
const citationListStyle: React.CSSProperties = {
  margin: '0.2rem 0 0',
  paddingLeft: '1.2rem',
  fontSize: '0.72rem',
}
const mutedStyle: React.CSSProperties = {
  margin: '0.3rem 0',
  color: 'var(--color-text-muted, #666)',
  fontSize: '0.78rem',
}
const mutedSpanStyle: React.CSSProperties = {
  color: 'var(--color-text-muted, #666)',
  fontSize: '0.74rem',
}
const codeStyle: React.CSSProperties = {
  fontFamily: 'ui-monospace, monospace',
  background: 'var(--color-surface-1, #fff)',
  padding: '0.05rem 0.3rem',
  borderRadius: '0.2rem',
}
const errorStyle: React.CSSProperties = {
  color: 'var(--color-danger-fg, #a83e2f)',
  margin: '0.3rem 0',
}
const refusalBoxStyle: React.CSSProperties = {
  background: 'var(--color-surface-danger, #fde)',
  border: '2px solid var(--color-danger-fg, #a83e2f)',
  borderRadius: '0.4rem',
  padding: '0.6rem 0.8rem',
  marginTop: '0.6rem',
}
const refusalHeadingStyle: React.CSSProperties = {
  margin: '0 0 0.3rem',
  fontWeight: 600,
}
const refusalBodyStyle: React.CSSProperties = {
  margin: '0.2rem 0 0.5rem',
}
const waiverFormStyle: React.CSSProperties = {
  display: 'flex',
  flexDirection: 'column',
  gap: '0.4rem',
  marginTop: '0.4rem',
}
const labelStyle: React.CSSProperties = {
  display: 'flex',
  flexDirection: 'column',
  gap: '0.2rem',
  fontSize: '0.76rem',
}
const inputStyle: React.CSSProperties = {
  padding: '0.3rem 0.5rem',
  fontSize: '0.78rem',
  border: '1px solid var(--color-border-subtle, #ccc)',
  borderRadius: '0.3rem',
}
const textareaStyle: React.CSSProperties = {
  padding: '0.3rem 0.5rem',
  fontSize: '0.78rem',
  border: '1px solid var(--color-border-subtle, #ccc)',
  borderRadius: '0.3rem',
  fontFamily: 'inherit',
  resize: 'vertical',
}
const buttonRowStyle: React.CSSProperties = {
  display: 'flex',
  gap: '0.4rem',
  justifyContent: 'flex-end',
}
const primaryButtonStyle: React.CSSProperties = {
  padding: '0.35rem 0.8rem',
  fontSize: '0.78rem',
  background: 'var(--color-danger-fg, #a83e2f)',
  color: '#fff',
  border: 'none',
  borderRadius: '0.3rem',
  cursor: 'pointer',
}
const secondaryButtonStyle: React.CSSProperties = {
  padding: '0.35rem 0.8rem',
  fontSize: '0.78rem',
  background: 'var(--color-surface-1, #fff)',
  color: 'var(--color-text-primary)',
  border: '1px solid var(--color-border-subtle, #ccc)',
  borderRadius: '0.3rem',
  cursor: 'pointer',
}
