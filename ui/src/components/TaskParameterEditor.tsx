/**
 * TaskParameterEditor — structured, per-type controls for an atom's
 * declared `ParameterSpec[]`. The SME sets concrete applied-parameter
 * overrides here (an *SME* action, never an assistant recommendation);
 * the parent drawer POSTs the collected `values` to the deterministic
 * `/task/:id/parameters` endpoint.
 *
 * Rendering by `ParameterType` (and by presence of `allowed_values`):
 *   - enum / any non-empty `allowed_values` → RadioRow (the codebase idiom)
 *   - boolean                                → two-option RadioRow (Yes/No)
 *   - integer / number                       → <input type="number">
 *   - string                                 → <input type="text">
 *   - array / object                         → <textarea> holding JSON,
 *     parsed on change; parse errors surface inline and suppress onChange
 *     until the JSON is valid again.
 *
 * Controlled component: each control reflects `values[name]` when set,
 * otherwise the spec `default`. No form library — plain hooks + inline
 * styles with `var(--color-*)` tokens only.
 *
 * Clear-vs-omit contract (mirrors the backend `apply_parameter_overrides`
 * semantics): blanking a control OR pressing its per-field "Clear" button
 * fires `onChange(name, null)` — a `null` value tells the backend to
 * REMOVE that override (reset to the step default). A field the SME never
 * touches emits no `onChange` at all, so the parent can omit it from the
 * submit payload (omitting a key KEEPS the existing override). The parent
 * distinguishes "cleared" (send null) from "untouched" (omit) by tracking
 * which names fired `onChange`.
 */

import { useEffect, useRef, useState } from 'react'
import type { ParameterSpec } from '../types/ParameterSpec'
import { RadioRow, type RadioOption } from './primitives/RadioRow'

interface Props {
  parameters: ParameterSpec[]
  /** Current SME-set values (`name -> value`). */
  values: Record<string, unknown>
  /**
   * Fired whenever a control changes. A `null` value means the SME
   * explicitly cleared the field (backend removes the override); any
   * other value sets it.
   */
  onChange: (name: string, value: unknown) => void
  /**
   * Lifted so the parent modal can disable Apply / Create-branch while
   * any array/object field holds unparseable JSON. Fired only when the
   * aggregate "has a parse error" boolean flips.
   */
  onValidityChange?: (hasErrors: boolean) => void
  disabled?: boolean
}

export default function TaskParameterEditor({
  parameters,
  values,
  onChange,
  onValidityChange,
  disabled,
}: Props): JSX.Element {
  // Raw textarea text + parse error, keyed by param name, for the
  // array/object JSON controls. Kept local so a transiently-invalid
  // edit stays in the box instead of being reverted by the parent.
  const [jsonDrafts, setJsonDrafts] = useState<Record<string, string>>({})
  const [jsonErrors, setJsonErrors] = useState<Record<string, string>>({})

  // Surface the aggregate "has invalid JSON" signal up to the parent so
  // it can gate submit. Only fire on a flip so an inline arrow prop
  // doesn't drive a re-render loop.
  const lastValidityRef = useRef<boolean | null>(null)
  useEffect(() => {
    const hasErr = Object.keys(jsonErrors).length > 0
    if (lastValidityRef.current !== hasErr) {
      lastValidityRef.current = hasErr
      onValidityChange?.(hasErr)
    }
  }, [jsonErrors, onValidityChange])

  // Reset a field to its cleared state: drop any local JSON draft/error
  // and tell the parent to remove the override (`null`).
  const clearField = (name: string) => {
    setJsonDrafts((d) => omit(d, name))
    setJsonErrors((er) => omit(er, name))
    onChange(name, null)
  }

  if (parameters.length === 0) {
    return (
      <p
        style={{
          margin: 0,
          fontSize: '0.82rem',
          color: 'var(--color-text-muted)',
          fontStyle: 'italic',
        }}
      >
        This step has no adjustable parameters.
      </p>
    )
  }

  const currentOf = (spec: ParameterSpec): unknown =>
    spec.name in values ? values[spec.name] : spec.default

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 14 }}>
      {parameters.map((spec) => {
        const current = currentOf(spec)
        const usesChoices =
          spec.type === 'enum' ||
          (Array.isArray(spec.allowed_values) && spec.allowed_values.length > 0)
        const hasClearableValue =
          current !== undefined &&
          current !== null &&
          !(typeof current === 'string' && current.trim() === '')

        return (
          <div key={spec.name} style={fieldStyle}>
            <div style={labelRowStyle}>
              <label htmlFor={`param-${spec.name}`} style={labelStyle}>
                {spec.name}
                {spec.required && (
                  <span
                    aria-hidden
                    title="required"
                    style={{ color: 'var(--color-danger-accent)', marginLeft: 3 }}
                  >
                    *
                  </span>
                )}
              </label>
              <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                {hasClearableValue && !disabled && (
                  <button
                    type="button"
                    onClick={() => clearField(spec.name)}
                    title="Remove this override (resets to the step's default)"
                    style={clearButtonStyle}
                  >
                    Clear
                  </button>
                )}
                <span style={typeChipStyle}>{spec.type}</span>
              </div>
            </div>
            {spec.description && (
              <p style={descStyle}>{spec.description}</p>
            )}

            {usesChoices ? (
              <ChoiceControl
                spec={spec}
                current={current}
                disabled={disabled}
                onChange={onChange}
              />
            ) : spec.type === 'boolean' ? (
              <BooleanControl
                spec={spec}
                current={current}
                disabled={disabled}
                onChange={onChange}
              />
            ) : spec.type === 'integer' || spec.type === 'number' ? (
              <input
                id={`param-${spec.name}`}
                type="number"
                step={spec.type === 'integer' ? 1 : 'any'}
                disabled={disabled}
                value={numberInputValue(current)}
                onChange={(e) => {
                  const raw = e.target.value
                  // Blanking a number field clears the override (null =
                  // remove), not "keep the old value".
                  onChange(spec.name, raw === '' ? null : Number(raw))
                }}
                style={inputStyle}
              />
            ) : spec.type === 'array' || spec.type === 'object' ? (
              <>
                <textarea
                  id={`param-${spec.name}`}
                  disabled={disabled}
                  rows={3}
                  value={
                    jsonDrafts[spec.name] !== undefined
                      ? jsonDrafts[spec.name]
                      : prettyJson(current)
                  }
                  onChange={(e) => {
                    const raw = e.target.value
                    setJsonDrafts((d) => ({ ...d, [spec.name]: raw }))
                    if (raw.trim() === '') {
                      setJsonErrors((er) => omit(er, spec.name))
                      // Emptying the box clears the override (null = remove).
                      onChange(spec.name, null)
                      return
                    }
                    try {
                      const parsed: unknown = JSON.parse(raw)
                      setJsonErrors((er) => omit(er, spec.name))
                      onChange(spec.name, parsed)
                    } catch (err) {
                      setJsonErrors((er) => ({
                        ...er,
                        [spec.name]: `Invalid JSON: ${(err as Error).message}`,
                      }))
                    }
                  }}
                  style={{ ...inputStyle, resize: 'vertical', fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace' }}
                  placeholder={spec.type === 'array' ? '[…]' : '{…}'}
                />
                {jsonErrors[spec.name] && (
                  <div role="alert" style={errorTextStyle}>
                    {jsonErrors[spec.name]}
                  </div>
                )}
              </>
            ) : (
              <input
                id={`param-${spec.name}`}
                type="text"
                disabled={disabled}
                value={stringInputValue(current)}
                onChange={(e) => onChange(spec.name, e.target.value)}
                style={inputStyle}
              />
            )}
          </div>
        )
      })}
    </div>
  )
}

function ChoiceControl({
  spec,
  current,
  disabled,
  onChange,
}: {
  spec: ParameterSpec
  current: unknown
  disabled?: boolean
  onChange: (name: string, value: unknown) => void
}): JSX.Element {
  const allowed = spec.allowed_values ?? []
  const selected = current === undefined || current === null ? null : String(current)
  return (
    <RadioRow<string>
      name={`param-${spec.name}`}
      ariaLabel={`${spec.name} value`}
      value={selected}
      onChange={(sel) => {
        // Map the stringified selection back to the original typed value
        // (allowed_values entries may be non-string JSON scalars).
        const orig = allowed.find((a) => String(a) === sel)
        onChange(spec.name, orig !== undefined ? orig : sel)
      }}
      options={allowed.map<RadioOption<string>>((a) => {
        const v = String(a)
        return { value: v, label: <span>{v}</span>, disabled }
      })}
    />
  )
}

function BooleanControl({
  spec,
  current,
  disabled,
  onChange,
}: {
  spec: ParameterSpec
  current: unknown
  disabled?: boolean
  onChange: (name: string, value: unknown) => void
}): JSX.Element {
  const selected =
    current === undefined || current === null ? null : String(Boolean(current))
  return (
    <RadioRow<string>
      name={`param-${spec.name}`}
      ariaLabel={`${spec.name} value`}
      value={selected}
      onChange={(sel) => onChange(spec.name, sel === 'true')}
      options={[
        { value: 'true', label: <span>Yes</span>, disabled },
        { value: 'false', label: <span>No</span>, disabled },
      ]}
    />
  )
}

function numberInputValue(current: unknown): number | string {
  if (typeof current === 'number' && Number.isFinite(current)) return current
  if (typeof current === 'string' && current.trim() !== '' && !Number.isNaN(Number(current))) {
    return Number(current)
  }
  return ''
}

function stringInputValue(current: unknown): string {
  if (current === undefined || current === null) return ''
  return typeof current === 'string' ? current : String(current)
}

function prettyJson(current: unknown): string {
  if (current === undefined || current === null) return ''
  try {
    return JSON.stringify(current, null, 2)
  } catch {
    return ''
  }
}

function omit(rec: Record<string, string>, key: string): Record<string, string> {
  if (!(key in rec)) return rec
  const next = { ...rec }
  delete next[key]
  return next
}

// ── styles ──────────────────────────────────────────────────────────

const fieldStyle: React.CSSProperties = {
  display: 'flex',
  flexDirection: 'column',
  gap: 5,
}
const labelRowStyle: React.CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  gap: 8,
  justifyContent: 'space-between',
}
const labelStyle: React.CSSProperties = {
  fontSize: '0.82rem',
  fontWeight: 600,
  color: 'var(--color-text-primary)',
  fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
}
const typeChipStyle: React.CSSProperties = {
  fontSize: '0.66rem',
  textTransform: 'uppercase',
  letterSpacing: '0.04em',
  color: 'var(--color-text-muted)',
  background: 'var(--color-surface-2)',
  padding: '1px 6px',
  borderRadius: 3,
}
const descStyle: React.CSSProperties = {
  margin: 0,
  fontSize: '0.76rem',
  color: 'var(--color-text-muted)',
  lineHeight: 1.4,
}
const inputStyle: React.CSSProperties = {
  width: '100%',
  padding: '0.45rem 0.6rem',
  borderRadius: 5,
  border: '1px solid var(--color-border-strong)',
  background: 'var(--color-surface-1)',
  color: 'var(--color-text-primary)',
  fontSize: '0.85rem',
  boxSizing: 'border-box',
}
const errorTextStyle: React.CSSProperties = {
  color: 'var(--color-danger-fg)',
  fontSize: '0.75rem',
}
const clearButtonStyle: React.CSSProperties = {
  border: 'none',
  background: 'transparent',
  color: 'var(--color-text-muted)',
  fontSize: '0.7rem',
  textDecoration: 'underline',
  cursor: 'pointer',
  padding: 0,
}
