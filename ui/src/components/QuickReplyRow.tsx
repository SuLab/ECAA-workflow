interface Props {
  options: string[]
  onPick: (option: string) => void
  disabled?: boolean
}

export default function QuickReplyRow({ options, onPick, disabled }: Props) {
  if (options.length === 0) return null
  return (
    <div
      role="group"
      aria-label="Quick reply options"
      style={{
        display: 'flex',
        flexWrap: 'wrap',
        gap: '0.4rem',
        marginTop: '0.6rem',
      }}
    >
      {options.map((opt) => (
        <button
          key={opt}
          type="button"
          onClick={() => onPick(opt)}
          disabled={disabled}
          style={{
            padding: '0.35rem 0.7rem',
            background: 'var(--color-surface-1)',
            border: '1px solid var(--color-border-strong)',
            borderRadius: 999,
            cursor: disabled ? 'not-allowed' : 'pointer',
            fontSize: '0.78rem',
            color: 'var(--color-text-primary)',
            opacity: disabled ? 0.5 : 1,
          }}
        >
          {opt}
        </button>
      ))}
    </div>
  )
}
