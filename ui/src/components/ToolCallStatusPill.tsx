interface Props {
  statusLine: string
}

export default function ToolCallStatusPill({ statusLine }: Props) {
  return (
    <div
      role="status"
      aria-live="polite"
      style={{
        display: 'inline-flex',
        alignItems: 'center',
        gap: '0.5rem',
        padding: '0.3rem 0.6rem',
        background: 'var(--color-info-bg)',
        border: '1px solid var(--color-info-border)',
        borderRadius: 999,
        fontSize: '0.78rem',
        color: 'var(--color-info-fg)',
        marginTop: '0.5rem',
        maxWidth: '100%',
      }}
    >
      <span
        aria-hidden="true"
        style={{
          width: 10,
          height: 10,
          borderRadius: '50%',
          background: 'var(--color-info-accent)',
          animation: 'scrippsPulse 1.2s ease-in-out infinite',
          flexShrink: 0,
        }}
      />
      <span>{statusLine}</span>
    </div>
  )
}
