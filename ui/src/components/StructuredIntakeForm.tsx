// MVP structured-intake form. Shown in place of the conversational
// ChatComposer when `GET /api/chat/llm-availability` returns anything
// other than `{ kind: "available" }`. The form gives the SME a path
// to start a deterministic session even when the LLM is disabled
// (operator kill-switch / no API key) or temporarily unavailable
// (transient 5xx / quota). v3 P10, closing v4 §6.4.
//
// The field set is intentionally small: goal, modality, organism,
// desired outputs, and uncertainties. The submit handler is wired by
// the parent so the same form drops into a "create new session" flow
// or a "branch from session N" flow without coupling to either.

import { useState } from 'react'

/**
 * The seven keyword-routable modalities supported by
 * `config/modality-keywords.yaml`. The fallback form omits the three
 * build-only modalities (`gwas-coloc`, `long-read-rnaseq`,
 * `spatial-transcriptomics`) because they are not classifier-reachable
 * from prose anyway — the SME would need to name a specific taxonomy
 * to use them, which means they have the developer surface already.
 */
export const MVP_MODALITY_OPTIONS = [
  { value: 'bulk_rnaseq', label: 'Bulk RNA-seq' },
  { value: 'single_cell_rnaseq', label: 'Single-cell RNA-seq' },
  { value: 'variant_calling', label: 'Variant calling' },
  { value: 'chip_seq', label: 'ChIP-seq' },
  { value: 'metagenomics', label: 'Metagenomics' },
  { value: 'proteomics', label: 'Proteomics' },
  { value: 'generic_omics', label: 'Other / not sure' },
] as const

/** v3 P10 structured intent shape captured by the MVP fallback form. */
export interface WorkflowIntent {
  goal: string
  modality: string
  organism: string
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
  const [modality, setModality] = useState('bulk_rnaseq')
  const [organism, setOrganism] = useState('')
  const [desiredOutputs, setDesiredOutputs] = useState('')
  const [uncertainties, setUncertainties] = useState('')
  const [submitting, setSubmitting] = useState(false)
  const [error, setError] = useState<string | null>(null)

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
        placeholder="e.g. Identify genes differentially expressed between treated and control."
        rows={3}
        required
        disabled={submitting || disabled}
        style={inputStyle}
      />

      <label htmlFor="intake-modality" style={labelStyle}>
        Modality
      </label>
      <select
        id="intake-modality"
        value={modality}
        onChange={(e) => setModality(e.target.value)}
        disabled={submitting || disabled}
        style={inputStyle}
      >
        {MVP_MODALITY_OPTIONS.map((opt) => (
          <option key={opt.value} value={opt.value}>
            {opt.label}
          </option>
        ))}
      </select>

      <label htmlFor="intake-organism" style={labelStyle}>
        Organism (optional)
      </label>
      <input
        id="intake-organism"
        type="text"
        value={organism}
        onChange={(e) => setOrganism(e.target.value)}
        placeholder="e.g. Homo sapiens, Mus musculus"
        disabled={submitting || disabled}
        style={inputStyle}
      />

      <label htmlFor="intake-outputs" style={labelStyle}>
        Desired outputs
      </label>
      <textarea
        id="intake-outputs"
        value={desiredOutputs}
        onChange={(e) => setDesiredOutputs(e.target.value)}
        placeholder="e.g. Differential-expression table, volcano plot, enrichment summary."
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
        placeholder="Anything you'd usually ask a bioinformatician?"
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
