// Structured-intake form. Shown in place of the conversational
// ChatComposer when `GET /api/chat/llm-availability` returns anything
// other than `{ kind: "available" }`. The form gives the SME a path
// to start a deterministic session even when the LLM is disabled
// (operator kill-switch / no API key) or temporarily unavailable
// (transient 5xx / quota). v3 P10, closing v4 §6.4.
//
// The field set is intentionally small: goal, modality, optional domain
// context, registered starting product, outputs, and uncertainties.
// Modality choices come from the server's runtime registry; the text
// input remains open so a newer server-side catalog is never constrained
// by a stale browser bundle.

import { useEffect, useState } from 'react'
import { getChatConfig } from '../api/chatClient'

/**
 * Safe fallback shown while `/api/chat/config` loads or when the UI is paired
 * with an older server. Runtime modalities are never duplicated in the browser
 * bundle. The field remains open text, so an operator can still enter an exact
 * registered id.
 */
export const FALLBACK_MODALITY_OPTIONS = [
  { value: 'auto', label: 'Auto-detect from goal' },
] as const

/** v3 P10 structured intent shape captured by the MVP fallback form. */
export interface WorkflowIntent {
  goal: string
  modality: string
  organism: string
  input_data_stage: string
  desired_outputs: string
  uncertainties: string
}

interface Props {
  onSubmit: (intent: WorkflowIntent) => Promise<void> | void
  disabled?: boolean
}

const labelStyle: React.CSSProperties = {
  display: 'block',
  fontSize: '0.78rem',
  fontWeight: 600,
  color: 'var(--color-text-default)',
  marginBottom: '0.3rem',
  marginTop: '0.85rem',
}

const inputStyle: React.CSSProperties = {
  width: '100%',
  padding: '0.5rem 0.7rem',
  background: 'var(--color-surface-2)',
  border: '1px solid var(--color-border-default)',
  borderRadius: 6,
  fontSize: '0.85rem',
  color: 'var(--color-text-default)',
  boxSizing: 'border-box',
}

export default function StructuredIntakeForm({ onSubmit, disabled }: Props) {
  const [goal, setGoal] = useState('')
  const [modality, setModality] = useState('auto')
  const [modalityOptions, setModalityOptions] = useState<
    Array<{ value: string; label: string }>
  >(FALLBACK_MODALITY_OPTIONS.slice())
  const [organism, setOrganism] = useState('')
  const [inputDataStage, setInputDataStage] = useState('')
  const [desiredOutputs, setDesiredOutputs] = useState('')
  const [uncertainties, setUncertainties] = useState('')
  const [submitting, setSubmitting] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let active = true
    void getChatConfig()
      .then((config) => {
        if (!active || !config.modalities?.length) return
        setModalityOptions(
          [
            { value: 'auto', label: 'Auto-detect from goal' },
            ...config.modalities.map((entry) => ({
              value: entry.id,
              label: entry.display_name,
            })),
          ],
        )
      })
      .catch(() => {
        // An older server may not include the catalog. The open text input and
        // compatibility suggestions remain usable.
      })
    return () => {
      active = false
    }
  }, [])

  const isValid = goal.trim().length > 0 && !!modality

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    if (!isValid || submitting || disabled) return
    setSubmitting(true)
    setError(null)
    try {
      await onSubmit({
        goal: goal.trim(),
        modality,
        organism: organism.trim(),
        input_data_stage: inputDataStage.trim(),
        desired_outputs: desiredOutputs.trim(),
        uncertainties: uncertainties.trim(),
      })
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to submit')
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <form
      onSubmit={handleSubmit}
      aria-label="Structured intake form"
      style={{
        padding: '1rem',
        background: 'var(--color-surface-1)',
        borderRadius: 8,
        maxWidth: 680,
      }}
    >
      <p
        style={{
          fontSize: '0.85rem',
          color: 'var(--color-text-muted)',
          marginTop: 0,
        }}
      >
        Describe your project. The compiler will build a workflow from these
        fields without needing the chat assistant.
      </p>

      <label htmlFor="intake-goal" style={labelStyle}>
        What are you trying to find out?
      </label>
      <textarea
        id="intake-goal"
        value={goal}
        onChange={(e) => setGoal(e.target.value)}
        placeholder="e.g. Estimate an effect, classify observations, or forecast future values."
        rows={3}
        required
        disabled={submitting || disabled}
        style={inputStyle}
      />

      <label htmlFor="intake-modality" style={labelStyle}>
        Modality or analysis family
      </label>
      <input
        id="intake-modality"
        type="text"
        list="intake-modality-options"
        value={modality}
        onChange={(e) => setModality(e.target.value)}
        required
        disabled={submitting || disabled}
        style={inputStyle}
      />
      <datalist id="intake-modality-options">
        {modalityOptions.map((opt) => (
          <option key={opt.value} value={opt.value}>
            {opt.label}
          </option>
        ))}
      </datalist>
      <div
        style={{
          marginTop: '0.3rem',
          fontSize: '0.74rem',
          color: 'var(--color-text-muted)',
        }}
      >
        Suggestions come from the current server catalog. For mixed or
        uncertain analyses, use auto and name every modality in the goal.
      </div>

      <label htmlFor="intake-organism" style={labelStyle}>
        Organism (if applicable)
      </label>
      <input
        id="intake-organism"
        type="text"
        value={organism}
        onChange={(e) => setOrganism(e.target.value)}
        placeholder="e.g. Homo sapiens, Mus musculus, or leave blank"
        disabled={submitting || disabled}
        style={inputStyle}
      />

      <label htmlFor="intake-data-stage" style={labelStyle}>
        Registered starting data product (optional)
      </label>
      <input
        id="intake-data-stage"
        type="text"
        value={inputDataStage}
        onChange={(e) => setInputDataStage(e.target.value)}
        placeholder="e.g. observation table, images, aligned reads, count matrix, called variants"
        disabled={submitting || disabled}
        style={inputStyle}
      />
      <div
        style={{
          marginTop: '0.3rem',
          fontSize: '0.74rem',
          color: 'var(--color-text-muted)',
        }}
      >
        Describe what the uploaded files are, not the method that produced
        them. This controls where the compiled workflow starts.
      </div>

      <label htmlFor="intake-outputs" style={labelStyle}>
        Desired outputs
      </label>
      <textarea
        id="intake-outputs"
        value={desiredOutputs}
        onChange={(e) => setDesiredOutputs(e.target.value)}
        placeholder="e.g. Result tables, diagnostics, figures, forecasts, and a final report."
        rows={3}
        disabled={submitting || disabled}
        style={inputStyle}
      />

      <label htmlFor="intake-uncertainties" style={labelStyle}>
        Open questions / uncertainties
      </label>
      <textarea
        id="intake-uncertainties"
        value={uncertainties}
        onChange={(e) => setUncertainties(e.target.value)}
        placeholder="Describe unresolved design choices, assumptions, constraints, or risks."
        rows={3}
        disabled={submitting || disabled}
        style={inputStyle}
      />

      {error && (
        <p
          role="alert"
          style={{
            color: 'var(--color-danger-fg)',
            fontSize: '0.8rem',
            marginTop: '0.7rem',
          }}
        >
          {error}
        </p>
      )}

      <button
        type="submit"
        disabled={!isValid || submitting || disabled}
        style={{
          marginTop: '1rem',
          padding: '0.55rem 1.1rem',
          background: 'var(--color-accent-bg, #3a7afe)',
          color: 'white',
          border: 'none',
          borderRadius: 6,
          fontWeight: 600,
          fontSize: '0.85rem',
          cursor: isValid && !submitting && !disabled ? 'pointer' : 'not-allowed',
          opacity: !isValid || submitting || disabled ? 0.6 : 1,
        }}
      >
        {submitting ? 'Submitting…' : 'Start workflow'}
      </button>
    </form>
  )
}
