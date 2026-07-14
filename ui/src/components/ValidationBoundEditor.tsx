/**
 * ValidationBoundEditor — a guided form the SME uses to author one
 * `SmeValidationBound` (a post-hoc, method-neutral constraint on a step's
 * *result*, never a method recommendation). The parent drawer POSTs the
 * composed bound to the deterministic `/task/:id/validation-bound` endpoint
 * (add flow) or stages it into a branch's `edits.validation_bounds`.
 *
 * Friendly check types map to the harness-runnable `assertion_type` set and
 * render only the conditional check fields that type consumes (mirroring
 * `crates/core/src/validation_bound.rs::validate_bound_check_shape`). An
 * "Advanced (raw JSON check)" option exposes the niche assertion types with a
 * raw `check` textarea. The editor is self-contained: it holds its field state
 * and emits `{ bound, valid }` up via `onChange` so the parent can gate submit.
 *
 * Controlled inputs only (no form library), inline styles with `var(--color-*)`
 * tokens, RadioRow for the severity choice — matching TaskParameterEditor.
 */

import { useEffect, useMemo, useRef, useState } from 'react'
import type { SmeValidationBound } from '../types/SmeValidationBound'
import { RadioRow, type RadioOption } from './primitives/RadioRow'

/** The composed bound plus whether it is well-formed enough to submit. */
export interface ValidationBoundDraft {
  bound: SmeValidationBound
  valid: boolean
}

/** Friendly check-type keys (the common types) plus the raw-JSON escape hatch. */
type CheckTypeKey =
  | 'numeric_threshold'
  | 'numeric_distribution'
  | 'reference_range_outlier'
  | 'artifact_present'
  | 'artifact_non_empty_table'
  | 'positive_control_present'
  | 'negative_control_present'
  | 'string_contains'
  | 'advanced'

/** Raw `assertion_type`s exposed only under the Advanced option. */
const ADVANCED_TYPES = [
  'json_pointer_is_bool',
  'json_pointer_is_array',
  'artifact_glob_any',
  'cross_stage_output_comparison',
  'cross_field_equals',
  'formula_references_covariates',
] as const
type AdvancedType = (typeof ADVANCED_TYPES)[number]

type OpKey = 'gte' | 'lte' | 'gt' | 'lt' | 'eq'
const OP_OPTIONS: { value: OpKey; label: string }[] = [
  { value: 'gte', label: '≥ (at least)' },
  { value: 'lte', label: '≤ (at most)' },
  { value: 'gt', label: '> (greater than)' },
  { value: 'lt', label: '< (less than)' },
  { value: 'eq', label: '= (equal to)' },
]
const OP_SYMBOL: Record<OpKey, string> = {
  gte: '≥',
  lte: '≤',
  gt: '>',
  lt: '<',
  eq: '=',
}

// Distribution statistics the harness `run_assertion` understands
// (crates/harness/src/main.rs numeric_distribution arm).
type StatKey = 'mean' | 'stdev' | 'skewness' | 'kurtosis' | 'p5' | 'p50' | 'p95'
const STAT_OPTIONS: { value: StatKey; label: string }[] = [
  { value: 'mean', label: 'mean' },
  { value: 'p50', label: 'median (p50)' },
  { value: 'p5', label: '5th percentile (p5)' },
  { value: 'p95', label: '95th percentile (p95)' },
  { value: 'stdev', label: 'std deviation' },
  { value: 'skewness', label: 'skewness' },
  { value: 'kurtosis', label: 'kurtosis' },
]

const CHECK_TYPE_OPTIONS: { value: CheckTypeKey; label: string; hint: string }[] = [
  {
    value: 'numeric_threshold',
    label: 'Numeric threshold',
    hint: 'A single number in the output must satisfy a comparison (e.g. adjusted p ≤ 0.01).',
  },
  {
    value: 'numeric_distribution',
    label: 'Distribution statistic',
    hint: 'A summary statistic over an array of numbers must satisfy a comparison.',
  },
  {
    value: 'reference_range_outlier',
    label: 'Reference range',
    hint: 'A value must fall within a reference minimum / maximum range.',
  },
  {
    value: 'artifact_present',
    label: 'Output file present',
    hint: 'The named output file must exist.',
  },
  {
    value: 'artifact_non_empty_table',
    label: 'Non-empty output table',
    hint: 'The named output table must exist and contain at least one row.',
  },
  {
    value: 'positive_control_present',
    label: 'Positive control present',
    hint: 'A positive-control value must be present at the given pointer.',
  },
  {
    value: 'negative_control_present',
    label: 'Negative control present',
    hint: 'A negative-control value must be present at the given pointer.',
  },
  {
    value: 'string_contains',
    label: 'Text contains',
    hint: 'The output text must contain the given phrase(s).',
  },
  {
    value: 'advanced',
    label: 'Advanced (raw JSON check)',
    hint: 'Pick a raw assertion type and edit the check JSON directly.',
  },
]

const ADVANCED_LABELS: Record<AdvancedType, string> = {
  json_pointer_is_bool: 'Pointer is a boolean',
  json_pointer_is_array: 'Pointer is an array',
  artifact_glob_any: 'Any file matches a glob',
  cross_stage_output_comparison: 'Compare against an upstream stage',
  cross_field_equals: 'Two fields must be equal',
  formula_references_covariates: 'Formula references covariates',
}

interface Props {
  /**
   * Resolved stage class the bound applies to (`spec.stage_class` ?? task id).
   * Written onto the bound and used to mint a stable id.
   */
  stageClass: string
  /**
   * Seed used to keep generated ids unique across bounds already attached to
   * the stage (or already staged in a branch). Usually the current count.
   */
  idSeed?: number
  /** Fired whenever the composed bound or its validity changes. */
  onChange: (draft: ValidationBoundDraft) => void
  disabled?: boolean
}

export default function ValidationBoundEditor({
  stageClass,
  idSeed = 0,
  onChange,
  disabled,
}: Props): JSX.Element {
  const [checkType, setCheckType] = useState<CheckTypeKey>('numeric_threshold')
  const [rawAssertionType, setRawAssertionType] = useState<AdvancedType>('json_pointer_is_bool')
  const [target, setTarget] = useState('')
  const [jsonPointer, setJsonPointer] = useState('')
  const [op, setOp] = useState<OpKey>('lte')
  const [value, setValue] = useState('')
  const [stat, setStat] = useState<StatKey>('mean')
  const [refMin, setRefMin] = useState('')
  const [refMax, setRefMax] = useState('')
  const [tolerance, setTolerance] = useState('')
  const [substringsText, setSubstringsText] = useState('')
  const [rawCheck, setRawCheck] = useState('{}')
  const [severity, setSeverity] = useState<'required' | 'recommended'>('required')
  const [description, setDescription] = useState('')
  const [descTouched, setDescTouched] = useState(false)

  const assertionType: string = checkType === 'advanced' ? rawAssertionType : checkType

  // Build the type-specific `check` payload (or a raw-JSON parse error).
  const { check, parseError } = useMemo<{
    check: Record<string, unknown> | null
    parseError: string | null
  }>(() => {
    const num = (s: string): number => (s.trim() === '' ? NaN : Number(s))
    const putNum = (o: Record<string, unknown>, k: string, s: string) => {
      const n = num(s)
      if (Number.isFinite(n)) o[k] = n
    }
    switch (checkType) {
      case 'artifact_present':
      case 'artifact_non_empty_table':
        return { check: null, parseError: null }
      case 'numeric_threshold': {
        const c: Record<string, unknown> = { op }
        if (jsonPointer.trim()) c.json_pointer = jsonPointer.trim()
        putNum(c, 'value', value)
        return { check: c, parseError: null }
      }
      case 'numeric_distribution': {
        const c: Record<string, unknown> = { op, stat }
        if (jsonPointer.trim()) c.json_pointer = jsonPointer.trim()
        putNum(c, 'value', value)
        return { check: c, parseError: null }
      }
      case 'reference_range_outlier': {
        const c: Record<string, unknown> = {}
        if (jsonPointer.trim()) c.json_pointer = jsonPointer.trim()
        putNum(c, 'reference_min', refMin)
        putNum(c, 'reference_max', refMax)
        if (tolerance.trim() !== '') putNum(c, 'tolerance', tolerance)
        return { check: c, parseError: null }
      }
      case 'positive_control_present':
      case 'negative_control_present': {
        const c: Record<string, unknown> = {}
        if (jsonPointer.trim()) c.json_pointer = jsonPointer.trim()
        return { check: c, parseError: null }
      }
      case 'string_contains': {
        const subs = substringsText
          .split('\n')
          .map((s) => s.trim())
          .filter((s) => s.length > 0)
        return { check: { substrings: subs }, parseError: null }
      }
      case 'advanced': {
        const trimmed = rawCheck.trim()
        if (trimmed === '') return { check: null, parseError: null }
        try {
          const parsed: unknown = JSON.parse(trimmed)
          if (parsed === null) return { check: null, parseError: null }
          if (typeof parsed !== 'object' || Array.isArray(parsed)) {
            return { check: null, parseError: 'The check must be a JSON object.' }
          }
          return { check: parsed as Record<string, unknown>, parseError: null }
        } catch (e) {
          return { check: null, parseError: `Invalid JSON: ${(e as Error).message}` }
        }
      }
      default:
        return { check: null, parseError: null }
    }
  }, [checkType, jsonPointer, op, value, stat, refMin, refMax, tolerance, substringsText, rawCheck])

  // Client-side shape validation mirroring validate_bound_check_shape.
  const shapeError = useMemo<string | null>(() => {
    if (target.trim() === '') return 'Add the output file path this check reads.'
    if (parseError) return parseError
    return validateCheckShape(assertionType, check)
  }, [target, parseError, assertionType, check])
  const valid = shapeError === null

  const autoDescription = useMemo(
    () => suggestDescription(checkType, assertionType, { target, jsonPointer, op, value, stat, refMin, refMax, substringsText }),
    [checkType, assertionType, target, jsonPointer, op, value, stat, refMin, refMax, substringsText],
  )
  const effectiveDescription = descTouched && description.trim() ? description : autoDescription

  const boundId = useMemo(
    () => makeBoundId(stageClass, assertionType, idSeed),
    [stageClass, assertionType, idSeed],
  )

  const bound: SmeValidationBound = useMemo(
    () => ({
      stage_class: stageClass,
      assertion_type: assertionType,
      target: target.trim(),
      check: check ?? null,
      severity,
      id: boundId,
      description: effectiveDescription.trim() || autoDescription,
    }),
    [stageClass, assertionType, target, check, severity, boundId, effectiveDescription, autoDescription],
  )

  // Fire onChange only when the emitted value actually changes so an inline
  // arrow prop on the parent can't drive a render loop.
  const lastSentRef = useRef<string>('')
  useEffect(() => {
    const key = JSON.stringify({ bound, valid })
    if (key !== lastSentRef.current) {
      lastSentRef.current = key
      onChange({ bound, valid })
    }
  }, [bound, valid, onChange])

  const showJsonPointer =
    checkType === 'numeric_threshold' ||
    checkType === 'numeric_distribution' ||
    checkType === 'reference_range_outlier' ||
    checkType === 'positive_control_present' ||
    checkType === 'negative_control_present'
  const showOpValue = checkType === 'numeric_threshold' || checkType === 'numeric_distribution'

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
      <Field label="Check type">
        <select
          data-testid="vb-check-type"
          value={checkType}
          disabled={disabled}
          onChange={(e) => setCheckType(e.target.value as CheckTypeKey)}
          style={selectStyle}
        >
          {CHECK_TYPE_OPTIONS.map((o) => (
            <option key={o.value} value={o.value}>
              {o.label}
            </option>
          ))}
        </select>
        <p style={hintStyle}>{CHECK_TYPE_OPTIONS.find((o) => o.value === checkType)?.hint}</p>
      </Field>

      {checkType === 'advanced' && (
        <Field label="Assertion type">
          <select
            data-testid="vb-advanced-type"
            value={rawAssertionType}
            disabled={disabled}
            onChange={(e) => setRawAssertionType(e.target.value as AdvancedType)}
            style={selectStyle}
          >
            {ADVANCED_TYPES.map((t) => (
              <option key={t} value={t}>
                {ADVANCED_LABELS[t]} ({t})
              </option>
            ))}
          </select>
        </Field>
      )}

      <Field label="Output file (relative path)">
        <input
          data-testid="vb-target"
          type="text"
          value={target}
          disabled={disabled}
          onChange={(e) => setTarget(e.target.value)}
          placeholder="results/tables/de.json"
          style={inputStyle}
        />
      </Field>

      {showJsonPointer && (
        <Field label="JSON pointer into that file">
          <input
            type="text"
            value={jsonPointer}
            disabled={disabled}
            onChange={(e) => setJsonPointer(e.target.value)}
            placeholder="/adjusted_p_max"
            style={inputStyle}
          />
        </Field>
      )}

      {checkType === 'numeric_distribution' && (
        <Field label="Statistic">
          <select
            value={stat}
            disabled={disabled}
            onChange={(e) => setStat(e.target.value as StatKey)}
            style={selectStyle}
          >
            {STAT_OPTIONS.map((o) => (
              <option key={o.value} value={o.value}>
                {o.label}
              </option>
            ))}
          </select>
        </Field>
      )}

      {showOpValue && (
        <div style={{ display: 'flex', gap: 10 }}>
          <Field label="Comparison" style={{ flex: 1 }}>
            <select
              value={op}
              disabled={disabled}
              onChange={(e) => setOp(e.target.value as OpKey)}
              style={selectStyle}
            >
              {OP_OPTIONS.map((o) => (
                <option key={o.value} value={o.value}>
                  {o.label}
                </option>
              ))}
            </select>
          </Field>
          <Field label="Value" style={{ flex: 1 }}>
            <input
              type="number"
              step="any"
              value={value}
              disabled={disabled}
              onChange={(e) => setValue(e.target.value)}
              placeholder="0.01"
              style={inputStyle}
            />
          </Field>
        </div>
      )}

      {checkType === 'reference_range_outlier' && (
        <div style={{ display: 'flex', gap: 10 }}>
          <Field label="Minimum" style={{ flex: 1 }}>
            <input
              type="number"
              step="any"
              value={refMin}
              disabled={disabled}
              onChange={(e) => setRefMin(e.target.value)}
              placeholder="0"
              style={inputStyle}
            />
          </Field>
          <Field label="Maximum" style={{ flex: 1 }}>
            <input
              type="number"
              step="any"
              value={refMax}
              disabled={disabled}
              onChange={(e) => setRefMax(e.target.value)}
              placeholder="1"
              style={inputStyle}
            />
          </Field>
          <Field label="Tolerance (optional)" style={{ flex: 1 }}>
            <input
              type="number"
              step="any"
              value={tolerance}
              disabled={disabled}
              onChange={(e) => setTolerance(e.target.value)}
              placeholder=""
              style={inputStyle}
            />
          </Field>
        </div>
      )}

      {checkType === 'string_contains' && (
        <Field label="Phrases to match (one per line)">
          <textarea
            value={substringsText}
            disabled={disabled}
            rows={3}
            onChange={(e) => setSubstringsText(e.target.value)}
            placeholder={'significant\nupregulated'}
            style={{ ...inputStyle, resize: 'vertical', fontFamily: 'inherit' }}
          />
        </Field>
      )}

      {checkType === 'advanced' && (
        <Field label="Check (raw JSON)">
          <textarea
            data-testid="vb-raw-check"
            value={rawCheck}
            disabled={disabled}
            rows={4}
            onChange={(e) => setRawCheck(e.target.value)}
            placeholder='{ "json_pointer": "/converged" }'
            style={{
              ...inputStyle,
              resize: 'vertical',
              fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
            }}
          />
        </Field>
      )}

      <Field label="Severity">
        <RadioRow<'required' | 'recommended'>
          name="vb-severity"
          ariaLabel="Severity"
          value={severity}
          onChange={(s) => setSeverity(s)}
          options={
            [
              { value: 'required', label: <span>Required (blocks the run)</span>, disabled },
              {
                value: 'recommended',
                label: <span>Recommended (warning only)</span>,
                disabled,
              },
            ] as RadioOption<'required' | 'recommended'>[]
          }
        />
      </Field>

      <Field label="Description (shown in the decision log)">
        <textarea
          value={effectiveDescription}
          disabled={disabled}
          rows={2}
          onChange={(e) => {
            setDescTouched(true)
            setDescription(e.target.value)
          }}
          style={{ ...inputStyle, resize: 'vertical', fontFamily: 'inherit' }}
        />
      </Field>

      {!valid && shapeError && (
        <div role="alert" style={errorTextStyle}>
          {shapeError}
        </div>
      )}
    </div>
  )
}

// ── ValidationBoundStager ───────────────────────────────────────────────────
//
// Lets the SME stage MULTIPLE bounds (for the branch-to-edit flow). Holds a
// composing editor plus the list of already-added bounds; the parent owns the
// committed list via `onBoundsChange`.

export function ValidationBoundStager({
  stageClass,
  baseSeed = 0,
  bounds,
  onBoundsChange,
  disabled,
}: {
  stageClass: string
  baseSeed?: number
  bounds: SmeValidationBound[]
  onBoundsChange: (next: SmeValidationBound[]) => void
  disabled?: boolean
}): JSX.Element {
  const [draft, setDraft] = useState<ValidationBoundDraft | null>(null)
  // Remount the editor after each add so its fields reset to defaults.
  const [editorKey, setEditorKey] = useState(0)

  const addDraft = () => {
    if (!draft?.valid) return
    onBoundsChange([...bounds, draft.bound])
    setDraft(null)
    setEditorKey((k) => k + 1)
  }
  const removeAt = (i: number) => {
    onBoundsChange(bounds.filter((_, idx) => idx !== i))
  }

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
      {bounds.length > 0 && (
        <ul style={stagedListStyle}>
          {bounds.map((b, i) => (
            <li key={b.id} style={stagedItemStyle}>
              <span style={{ minWidth: 0, flex: 1 }}>
                <SeverityChip severity={b.severity} />{' '}
                {b.description || `${b.assertion_type} on ${b.target}`}
              </span>
              {!disabled && (
                <button
                  type="button"
                  onClick={() => removeAt(i)}
                  style={smallLinkButtonStyle}
                  aria-label={`Remove ${b.description || b.id}`}
                >
                  Remove
                </button>
              )}
            </li>
          ))}
        </ul>
      )}
      <ValidationBoundEditor
        key={editorKey}
        stageClass={stageClass}
        idSeed={baseSeed + bounds.length}
        onChange={setDraft}
        disabled={disabled}
      />
      <div>
        <button
          type="button"
          data-testid="vb-add-to-branch"
          onClick={addDraft}
          disabled={disabled || !draft?.valid}
          style={{
            ...addButtonStyle,
            opacity: !disabled && draft?.valid ? 1 : 0.55,
            cursor: !disabled && draft?.valid ? 'pointer' : 'not-allowed',
          }}
        >
          Add this check
        </button>
      </div>
    </div>
  )
}

export function SeverityChip({ severity }: { severity: string }): JSX.Element {
  const required = severity === 'required'
  return (
    <span
      style={{
        display: 'inline-block',
        padding: '0px 6px',
        borderRadius: 3,
        fontSize: '0.66rem',
        fontWeight: 700,
        textTransform: 'uppercase',
        letterSpacing: '0.03em',
        background: required ? 'var(--color-danger-bg)' : 'var(--color-surface-2)',
        color: required ? 'var(--color-danger-fg)' : 'var(--color-text-secondary)',
      }}
    >
      {required ? 'Required' : 'Recommended'}
    </span>
  )
}

// ── pure helpers (exported for reuse / testing) ──────────────────────────────

/** Slugify a stage class for use inside a bound id. */
function slug(s: string): string {
  return (
    s
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, '_')
      .replace(/^_+|_+$/g, '')
      .slice(0, 40) || 'stage'
  )
}

/** Mint a stable, unique-ish bound id. */
export function makeBoundId(stageClass: string, assertionType: string, seed: number): string {
  return `sme_${slug(stageClass)}_${assertionType}_${seed}`
}

/**
 * Client-side mirror of
 * `crates/core/src/validation_bound.rs::validate_bound_check_shape`. Returns a
 * human message on the first missing/typed field, or `null` when well-formed.
 * Stricter than the server on emptiness (requires non-empty strings) so the SME
 * can't compose a payload the server would 400.
 */
export function validateCheckShape(
  assertionType: string,
  check: Record<string, unknown> | null,
): string | null {
  const hasStr = (k: string): boolean => {
    const v = check?.[k]
    return typeof v === 'string' && v.trim().length > 0
  }
  const hasNum = (k: string): boolean => {
    const v = check?.[k]
    return typeof v === 'number' && Number.isFinite(v)
  }
  const hasArr = (k: string): boolean => {
    const v = check?.[k]
    return Array.isArray(v) && v.length > 0
  }
  const need = (missing: string[]): string | null =>
    missing.length ? `Fill in: ${missing.join(', ')}.` : null

  switch (assertionType) {
    case 'artifact_present':
    case 'artifact_non_empty_table':
    case 'artifact_glob_any':
      return null
    case 'string_contains':
      return hasArr('substrings') || hasArr('substrings_any')
        ? null
        : 'Add at least one phrase to match.'
    case 'numeric_threshold': {
      const m: string[] = []
      if (!hasStr('json_pointer')) m.push('a JSON pointer')
      if (!hasStr('op')) m.push('a comparison')
      if (!hasNum('value')) m.push('a numeric value')
      return need(m)
    }
    case 'numeric_distribution': {
      const m: string[] = []
      if (!hasStr('json_pointer')) m.push('a JSON pointer')
      if (!hasStr('stat')) m.push('a statistic')
      if (!hasStr('op')) m.push('a comparison')
      if (!hasNum('value')) m.push('a numeric value')
      return need(m)
    }
    case 'reference_range_outlier': {
      const m: string[] = []
      if (!hasStr('json_pointer')) m.push('a JSON pointer')
      if (!hasNum('reference_min')) m.push('a minimum')
      if (!hasNum('reference_max')) m.push('a maximum')
      return need(m)
    }
    case 'positive_control_present':
    case 'negative_control_present':
    case 'json_pointer_is_bool':
    case 'json_pointer_is_array':
      return hasStr('json_pointer') ? null : 'Add a JSON pointer.'
    case 'cross_stage_output_comparison': {
      const m: string[] = []
      if (!hasStr('this_pointer')) m.push('this_pointer')
      if (!hasStr('upstream_task')) m.push('upstream_task')
      if (!hasStr('upstream_pointer')) m.push('upstream_pointer')
      if (!hasStr('op')) m.push('op')
      return need(m)
    }
    case 'cross_field_equals': {
      const m: string[] = []
      if (!hasStr('this_pointer')) m.push('this_pointer')
      if (!hasStr('other_pointer')) m.push('other_pointer')
      return need(m)
    }
    case 'formula_references_covariates': {
      const m: string[] = []
      if (!hasStr('formula_pointer')) m.push('formula_pointer')
      if (!hasStr('covariates_pointer')) m.push('covariates_pointer')
      if (!hasStr('primary_pointer')) m.push('primary_pointer')
      return need(m)
    }
    default:
      return 'Unsupported check type.'
  }
}

function suggestDescription(
  checkType: CheckTypeKey,
  assertionType: string,
  f: {
    target: string
    jsonPointer: string
    op: OpKey
    value: string
    stat: StatKey
    refMin: string
    refMax: string
    substringsText: string
  },
): string {
  const ptr = f.jsonPointer.trim() || 'the value'
  const tgt = f.target.trim() || 'the output'
  const val = f.value.trim() || '?'
  switch (checkType) {
    case 'numeric_threshold':
      return `SME: ${ptr} ${OP_SYMBOL[f.op]} ${val}`
    case 'numeric_distribution':
      return `SME: ${f.stat} of ${ptr} ${OP_SYMBOL[f.op]} ${val}`
    case 'reference_range_outlier':
      return `SME: ${ptr} within [${f.refMin.trim() || '?'}, ${f.refMax.trim() || '?'}]`
    case 'artifact_present':
      return `SME: ${tgt} must exist`
    case 'artifact_non_empty_table':
      return `SME: ${tgt} must be a non-empty table`
    case 'positive_control_present':
      return `SME: positive control present at ${ptr}`
    case 'negative_control_present':
      return `SME: negative control present at ${ptr}`
    case 'string_contains': {
      const subs = f.substringsText
        .split('\n')
        .map((s) => s.trim())
        .filter(Boolean)
      return `SME: ${tgt} contains ${subs.length ? subs.join(', ') : '…'}`
    }
    default:
      return `SME: ${assertionType} on ${tgt}`
  }
}

// ── small presentational subcomponents + styles ──────────────────────────────

function Field({
  label,
  children,
  style,
}: {
  label: string
  children: React.ReactNode
  style?: React.CSSProperties
}): JSX.Element {
  return (
    <label style={{ display: 'flex', flexDirection: 'column', gap: 4, ...style }}>
      <span style={labelStyle}>{label}</span>
      {children}
    </label>
  )
}

const labelStyle: React.CSSProperties = {
  fontSize: '0.78rem',
  color: 'var(--color-text-secondary)',
  fontWeight: 500,
}
const hintStyle: React.CSSProperties = {
  margin: 0,
  fontSize: '0.74rem',
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
const selectStyle: React.CSSProperties = {
  ...inputStyle,
  cursor: 'pointer',
}
const errorTextStyle: React.CSSProperties = {
  color: 'var(--color-danger-fg)',
  fontSize: '0.76rem',
}
const stagedListStyle: React.CSSProperties = {
  margin: 0,
  padding: 0,
  listStyle: 'none',
  display: 'flex',
  flexDirection: 'column',
  gap: 6,
}
const stagedItemStyle: React.CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  gap: 8,
  fontSize: '0.82rem',
  color: 'var(--color-text-primary)',
  background: 'var(--color-surface-0)',
  border: '1px solid var(--color-border-default)',
  borderRadius: 5,
  padding: '6px 8px',
}
const smallLinkButtonStyle: React.CSSProperties = {
  border: 'none',
  background: 'transparent',
  color: 'var(--color-danger-accent)',
  fontSize: '0.74rem',
  textDecoration: 'underline',
  cursor: 'pointer',
  padding: 0,
  flexShrink: 0,
}
const addButtonStyle: React.CSSProperties = {
  padding: '0.4rem 0.8rem',
  borderRadius: 6,
  border: '1px solid var(--color-border-strong)',
  background: 'var(--color-surface-1)',
  color: 'var(--color-text-secondary)',
  fontWeight: 500,
  fontSize: '0.8rem',
}
