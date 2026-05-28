import type { ReactNode } from 'react'

export interface RadioOption<T extends string> {
  value: T
  label: ReactNode
  disabled?: boolean
  /**
   * Optional accessible name for the radio input itself. When omitted,
   * the input's accessible name falls through to the rendered label
   * ReactNode. Use this when the visible label contains decorative
   * annotations (score chips, badges) that would clutter the screen
   * reader announcement.
   */
  ariaLabel?: string
}

interface RadioRowProps<T extends string> {
  name: string
  options: RadioOption<T>[]
  value: T | null
  onChange: (next: T) => void
  ariaLabel?: string
}

/**
 * Shared radio list used by the selection cards (BlockerCard
 * discovery-approval, sensitivity comparison, etc.). Each option is
 * one `<label>` containing a radio input plus its rendered label
 * (which may be text or any ReactNode so consumers can layer score
 * chips / annotations).
 */
export function RadioRow<T extends string>({
  name,
  options,
  value,
  onChange,
  ariaLabel,
}: RadioRowProps<T>): JSX.Element {
  return (
    <div
      role="radiogroup"
      aria-label={ariaLabel ?? name}
      style={{ display: 'flex', flexDirection: 'column', gap: '0.25rem' }}
    >
      {options.map((opt) => (
        <label
          key={opt.value}
          style={{
            display: 'flex',
            alignItems: 'center',
            gap: '0.5rem',
            padding: '0.25rem 0',
            fontSize: '0.82rem',
            color: 'var(--color-text-primary)',
            cursor: opt.disabled ? 'not-allowed' : 'pointer',
            opacity: opt.disabled ? 0.6 : 1,
          }}
        >
          <input
            type="radio"
            name={name}
            value={opt.value}
            checked={value === opt.value}
            onChange={() => onChange(opt.value)}
            disabled={opt.disabled}
            aria-label={opt.ariaLabel}
          />
          {opt.label}
        </label>
      ))}
    </div>
  )
}
