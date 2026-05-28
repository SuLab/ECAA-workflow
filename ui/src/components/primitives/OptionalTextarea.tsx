interface OptionalTextareaProps {
  label: string
  placeholder?: string
  value: string
  onChange: (next: string) => void
  disabled?: boolean
  rows?: number
  ariaLabel?: string
}

/**
 * Shared rationale/notes textarea used by the 5 cards that collect an
 * optional comment (branch, rerun, amend, sensitivity, unblock).
 * Collapses hand-rolled label + textarea + style block into one
 * primitive.
 */
export function OptionalTextarea({
  label,
  placeholder,
  value,
  onChange,
  disabled,
  rows = 3,
  ariaLabel,
}: OptionalTextareaProps): JSX.Element {
  return (
    <label
      style={{
        display: 'flex',
        flexDirection: 'column',
        gap: '0.25rem',
        fontSize: '0.78rem',
        color: 'var(--color-text-secondary)',
        marginBottom: '0.6rem',
      }}
    >
      <span>{label}</span>
      <textarea
        aria-label={ariaLabel ?? label}
        placeholder={placeholder}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        disabled={disabled}
        rows={rows}
        style={{
          padding: '0.4rem 0.6rem',
          border: '1px solid var(--color-border-strong)',
          background: 'var(--color-surface-1)',
          color: 'var(--color-text-primary)',
          borderRadius: 6,
          fontFamily: 'inherit',
          fontSize: '0.82rem',
          resize: 'vertical',
          opacity: disabled ? 0.6 : 1,
        }}
      />
    </label>
  )
}
