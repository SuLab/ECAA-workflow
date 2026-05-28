/**
 * v4 P6 / D4 — `GraduationCandidateCard` UI component (design v4 §4.2).
 *
 * Renders the cross-session `LocalExtension` graduation candidates
 * surfaced by `GET /api/chat/session/:id/graduation/candidates`. Each
 * row carries:
 *  - The IRI + human label of the candidate
 *  - Usage counters (usage_count, unique_sessions, success_rate)
 *  - The target upstream ontology the candidate would graduate to
 *  - A small "Annotate for upstream submission" form
 *
 * Posting the annotation form fires
 * `POST /api/chat/session/:id/graduation/:iri/annotate` with
 * `{annotated_by, submission_ref, rationale}`. The handler records the
 * annotation onto the session decision-log; the upstream submission
 * itself is operator work outside the chat surface.
 */

import { useState } from 'react'

export interface GraduationThresholds {
  min_usage_count: number
  min_unique_sessions: number
  min_success_rate: number
}

export interface GraduationCandidateSummary {
  iri: string
  label: string
  usage_count: number
  unique_sessions: number
  success_rate: number
  graduation_target_ontology: string
}

export interface GraduationCandidatesResponse {
  thresholds: GraduationThresholds
  candidates: GraduationCandidateSummary[]
}

interface Props {
  data: GraduationCandidatesResponse
  /**
   * Optional click handler. When provided, candidate rows show the
   * annotation form; on submit the parent posts the body to
   * `/api/chat/session/:id/graduation/:iri/annotate`. When omitted,
   * the card is read-only (e.g. on shared/snapshot views).
   */
  onAnnotate?: (iri: string, annotatedBy: string, submissionRef: string, rationale: string) => void
}

export default function GraduationCandidateCard({ data, onAnnotate }: Props) {
  const { thresholds, candidates } = data
  if (candidates.length === 0) {
    return (
      <div role="region" aria-label="LocalExtension graduation candidates" style={emptyStyle}>
        No graduation candidates yet. Thresholds: {thresholds.min_usage_count} usages across{' '}
        {thresholds.min_unique_sessions} sessions at{' '}
        {(thresholds.min_success_rate * 100).toFixed(0)}% success.
      </div>
    )
  }
  return (
    <div role="region" aria-label="LocalExtension graduation candidates" style={cardStyle}>
      <h4 style={headingStyle}>
        {candidates.length} graduation candidate{candidates.length === 1 ? '' : 's'}
      </h4>
      <p style={legendStyle}>
        LocalExtensions that have crossed the graduation thresholds (
        {thresholds.min_usage_count} usages, {thresholds.min_unique_sessions} sessions,{' '}
        {(thresholds.min_success_rate * 100).toFixed(0)}% success). Annotate any for upstream
        ontology submission.
      </p>
      <table style={tableStyle}>
        <thead>
          <tr>
            <th style={thStyle}>IRI</th>
            <th style={thStyle}>Label</th>
            <th style={thStyleNumeric}>Usage</th>
            <th style={thStyleNumeric}>Sessions</th>
            <th style={thStyleNumeric}>Success</th>
            <th style={thStyle}>Target</th>
            <th style={thStyle}>{onAnnotate ? 'Annotate' : ''}</th>
          </tr>
        </thead>
        <tbody>
          {candidates.map((c) => (
            <CandidateRow key={c.iri} candidate={c} onAnnotate={onAnnotate} />
          ))}
        </tbody>
      </table>
    </div>
  )
}

function CandidateRow({
  candidate,
  onAnnotate,
}: {
  candidate: GraduationCandidateSummary
  onAnnotate?: (iri: string, annotatedBy: string, submissionRef: string, rationale: string) => void
}) {
  const [annotatedBy, setAnnotatedBy] = useState('')
  const [submissionRef, setSubmissionRef] = useState('')
  const [rationale, setRationale] = useState('')
  const [open, setOpen] = useState(false)
  return (
    <>
      <tr>
        <td style={tdCodeStyle}>{candidate.iri}</td>
        <td style={tdStyle}>{candidate.label}</td>
        <td style={tdStyleNumeric}>{candidate.usage_count}</td>
        <td style={tdStyleNumeric}>{candidate.unique_sessions}</td>
        <td style={tdStyleNumeric}>{(candidate.success_rate * 100).toFixed(0)}%</td>
        <td style={tdStyle}>{candidate.graduation_target_ontology}</td>
        <td style={tdStyle}>
          {onAnnotate && (
            <button
              type="button"
              onClick={() => setOpen((v) => !v)}
              style={annotateToggleStyle}
              aria-expanded={open}
              aria-label={`Annotate ${candidate.iri} for upstream submission`}
            >
              {open ? 'Close' : 'Annotate'}
            </button>
          )}
        </td>
      </tr>
      {open && onAnnotate && (
        <tr>
          <td colSpan={7} style={formCellStyle}>
            <div style={formStyle}>
              <input
                type="text"
                placeholder="annotated_by (your name / id)"
                value={annotatedBy}
                onChange={(e) => setAnnotatedBy(e.target.value)}
                style={inputStyle}
                aria-label="Annotator name or id"
              />
              <input
                type="text"
                placeholder="submission_ref (URL / issue id, optional)"
                value={submissionRef}
                onChange={(e) => setSubmissionRef(e.target.value)}
                style={inputStyle}
                aria-label="Upstream submission reference"
              />
              <input
                type="text"
                placeholder="rationale (optional)"
                value={rationale}
                onChange={(e) => setRationale(e.target.value)}
                style={inputStyle}
                aria-label="Annotation rationale"
              />
              <button
                type="button"
                onClick={() => {
                  onAnnotate(candidate.iri, annotatedBy, submissionRef, rationale)
                  setAnnotatedBy('')
                  setSubmissionRef('')
                  setRationale('')
                  setOpen(false)
                }}
                disabled={!annotatedBy.trim()}
                style={submitStyle}
              >
                Submit annotation
              </button>
            </div>
          </td>
        </tr>
      )}
    </>
  )
}

const cardStyle: React.CSSProperties = {
  padding: '0.6rem 0.85rem',
  fontSize: '0.78rem',
  background: 'var(--color-surface-muted)',
  border: '1px solid var(--color-border-subtle)',
  borderRadius: '0.4rem',
}
const emptyStyle: React.CSSProperties = {
  padding: '0.6rem 0.85rem',
  fontSize: '0.78rem',
  color: 'var(--color-text-muted)',
  fontStyle: 'italic',
}
const headingStyle: React.CSSProperties = {
  margin: '0 0 0.4rem',
  fontSize: '0.8rem',
  fontWeight: 600,
}
const legendStyle: React.CSSProperties = {
  margin: '0 0 0.5rem',
  fontSize: '0.72rem',
  color: 'var(--color-text-muted)',
  fontStyle: 'italic',
}
const tableStyle: React.CSSProperties = {
  width: '100%',
  borderCollapse: 'collapse',
  fontSize: '0.74rem',
}
const thStyle: React.CSSProperties = {
  textAlign: 'left',
  padding: '0.25rem 0.4rem',
  borderBottom: '1px solid var(--color-border-subtle)',
  fontWeight: 600,
}
const thStyleNumeric: React.CSSProperties = {
  ...thStyle,
  textAlign: 'right',
}
const tdStyle: React.CSSProperties = {
  padding: '0.25rem 0.4rem',
  borderBottom: '1px solid var(--color-border-faint)',
}
const tdCodeStyle: React.CSSProperties = {
  ...tdStyle,
  fontFamily: 'ui-monospace, monospace',
}
const tdStyleNumeric: React.CSSProperties = {
  ...tdStyle,
  textAlign: 'right',
  fontVariantNumeric: 'tabular-nums',
}
const annotateToggleStyle: React.CSSProperties = {
  padding: '0.2rem 0.5rem',
  fontSize: '0.7rem',
  background: 'var(--color-info-accent)',
  color: '#fff',
  border: 'none',
  borderRadius: '0.25rem',
  cursor: 'pointer',
}
const formCellStyle: React.CSSProperties = {
  padding: '0.5rem 0.4rem',
  background: 'var(--color-surface-1)',
}
const formStyle: React.CSSProperties = {
  display: 'flex',
  gap: '0.3rem',
  flexWrap: 'wrap',
  alignItems: 'center',
}
const inputStyle: React.CSSProperties = {
  padding: '0.25rem 0.4rem',
  fontSize: '0.72rem',
  border: '1px solid var(--color-border-subtle)',
  borderRadius: '0.25rem',
  background: 'var(--color-surface-muted)',
  color: 'inherit',
  flex: '1 1 12rem',
  minWidth: '8rem',
}
const submitStyle: React.CSSProperties = {
  padding: '0.3rem 0.6rem',
  fontSize: '0.72rem',
  background: 'var(--color-success-accent, #2a8c5d)',
  color: '#fff',
  border: 'none',
  borderRadius: '0.3rem',
  cursor: 'pointer',
}
