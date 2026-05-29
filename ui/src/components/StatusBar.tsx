interface Props {
  mode: 'idle' | 'planning' | 'executing'
  modality?: string
  packagePath?: string
  onReset: () => void
}

const MODE_LABEL = { idle: 'Idle', planning: 'Planning', executing: 'Executing' } as const
const MODE_COLOR: Record<Props['mode'], string> = {
  idle: 'var(--color-text-muted)',
  planning: 'var(--color-accent)',
  executing: 'var(--color-success-accent)',
}

export default function StatusBar({ mode, modality, packagePath, onReset }: Props) {
  return (
    <div style={{
      display: 'flex', alignItems: 'center', gap: '0.75rem',
      padding: '0.45rem 1rem',
      background: 'var(--color-chrome-bg-elevated)', color: 'var(--color-chrome-fg)',
      fontSize: '0.83rem', borderBottom: '1px solid var(--color-chrome-border-strong)',
      flexShrink: 0,
    }}>
      <span style={{ fontWeight: 700, color: 'var(--color-chrome-fg)', letterSpacing: '-0.01em' }}>
        ECAA-workflow
      </span>
      <span style={{
        padding: '2px 7px', borderRadius: 4,
        background: MODE_COLOR[mode], color: 'var(--color-text-on-accent)',
        fontWeight: 600, fontSize: '0.7rem', textTransform: 'uppercase',
      }}>
        {MODE_LABEL[mode]}
      </span>
      {modality && (
        <span style={{ color: 'var(--color-chrome-fg-faint)' }}>
          modality: <strong style={{ color: 'var(--color-chrome-fg)' }}>{modality}</strong>
        </span>
      )}
      {packagePath && (
        <span style={{ color: 'var(--color-chrome-fg-faint)' }}>
          pkg: <code style={{ fontSize: '0.78rem', color: 'var(--color-chrome-fg-accent)' }}>{packagePath}</code>
        </span>
      )}
      <button
        onClick={onReset}
        style={{
          marginLeft: 'auto', padding: '2px 10px',
          background: 'transparent', border: '1px solid var(--color-chrome-border-strong)',
          borderRadius: 4, color: 'var(--color-chrome-fg-muted)',
          cursor: 'pointer', fontSize: '0.78rem',
        }}
      >
        Reset
      </button>
    </div>
  )
}
