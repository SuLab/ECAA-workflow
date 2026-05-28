import { useState } from 'react'
import { useAsync } from '../hooks/useAsync'
import { CardContainer } from './primitives/CardContainer'
import { SubmitCancelRow } from './primitives/SubmitCancelRow'

export interface StructuredCaptureField {
  key: string
  label: string
  placeholder?: string
  required?: boolean
  multiline?: boolean
}

interface Props {
  title: string
  description?: string
  fields: StructuredCaptureField[]
  initialValues?: Record<string, string>
  onSubmit: (values: Record<string, string>) => void | Promise<void>
  onCancel?: () => void
}

/**
 * Inline structured capture card — used for dense input that exceeds what a
 * single freeform composer turn can comfortably capture (per spec §9.2 rule 4:
 * "higher-density structured input, for example >6 fields or multi-step
 * validation"). The card lives inside the conversation timeline rather than
 * a modal so transcript continuity is preserved.
 *
 * Used narrowly for cases like "study accessions with per-study
 * metadata"; broader uses are gated on real demand.
 */
export default function StructuredCaptureTurnCard({
  title,
  description,
  fields,
  initialValues,
  onSubmit,
  onCancel,
}: Props) {
  const [values, setValues] = useState<Record<string, string>>(
    () => initialValues ?? {},
  )
  // Two error sources: (a) sync required-field validation that short-
  // circuits before any async work, (b) async failure from onSubmit.
  // useAsync covers (b); local state covers (a). Render whichever is
  // non-null.
  const [validationError, setValidationError] = useState<string | null>(null)
  const { busy: submitting, error: asyncError, run } = useAsync()
  const error = validationError ?? asyncError

  const updateField = (key: string, value: string) =>
    setValues((prev) => ({ ...prev, [key]: value }))

  const handleSubmit = async () => {
    const missing = fields
      .filter((f) => f.required && !values[f.key]?.trim())
      .map((f) => f.label)
    if (missing.length > 0) {
      setValidationError(`Please fill in: ${missing.join(', ')}`)
      return
    }
    setValidationError(null)
    await run(() => Promise.resolve(onSubmit(values)))
  }

  return (
    <CardContainer palette="neutral" title={title} ariaLabel={title}>
      {description && (
        <p
          style={{
            margin: '0 0 0.75rem 0',
            fontSize: '0.8rem',
            color: 'var(--color-text-secondary)',
            lineHeight: 1.5,
          }}
        >
          {description}
        </p>
      )}
      <div style={{ display: 'flex', flexDirection: 'column', gap: '0.6rem' }}>
        {fields.map((field) => {
          const id = `structured-${field.key}`
          return (
            <div key={field.key} style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
              <label
                htmlFor={id}
                style={{
                  fontSize: '0.78rem',
                  color: 'var(--color-text-secondary)',
                  fontWeight: 500,
                }}
              >
                {field.label}
                {field.required && (
                  <span aria-hidden="true" style={{ color: 'var(--color-danger-accent)', marginLeft: 4 }}>
                    *
                  </span>
                )}
              </label>
              {field.multiline ? (
                <textarea
                  id={id}
                  rows={3}
                  required={field.required}
                  value={values[field.key] ?? ''}
                  onChange={(e) => updateField(field.key, e.target.value)}
                  placeholder={field.placeholder}
                  style={{
                    padding: '0.4rem 0.55rem',
                    border: '1px solid var(--color-border-strong)',
                    borderRadius: 4,
                    fontSize: '0.83rem',
                    fontFamily: 'inherit',
                    resize: 'vertical',
                    outline: 'none',
                    background: 'var(--color-surface-1)',
                    color: 'var(--color-text-primary)',
                  }}
                />
              ) : (
                <input
                  id={id}
                  type="text"
                  required={field.required}
                  value={values[field.key] ?? ''}
                  onChange={(e) => updateField(field.key, e.target.value)}
                  placeholder={field.placeholder}
                  style={{
                    padding: '0.4rem 0.55rem',
                    border: '1px solid var(--color-border-strong)',
                    borderRadius: 4,
                    fontSize: '0.83rem',
                    fontFamily: 'inherit',
                    outline: 'none',
                    background: 'var(--color-surface-1)',
                    color: 'var(--color-text-primary)',
                  }}
                />
              )}
            </div>
          )
        })}
      </div>
      {error && (
        <CardContainer
          palette="danger"
          role="alert"
          style={{
            marginTop: '0.6rem',
            padding: '0.4rem 0.55rem',
            color: 'var(--color-danger-fg)',
            fontSize: '0.78rem',
            borderLeft: '1px solid var(--color-danger-border)',
          }}
        >
          {error}
        </CardContainer>
      )}
      <div style={{ marginTop: '0.85rem' }}>
        <SubmitCancelRow
          palette="neutral"
          submitLabel="Submit"
          cancelLabel="Cancel"
          onSubmit={handleSubmit}
          onCancel={onCancel}
          busy={submitting}
        />
      </div>
    </CardContainer>
  )
}
