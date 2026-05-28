import { Component, type ReactNode } from 'react'

interface Props {
  fallbackLabel: string
  children: ReactNode
}

interface State {
  error: Error | null
}

export default class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null }

  static getDerivedStateFromError(error: Error): State {
    return { error }
  }

  componentDidCatch(error: Error): void {
    console.error('[ErrorBoundary]', error)
  }

  render() {
    if (this.state.error) {
      return (
        <div
          role="alert"
          style={{
            padding: '1rem',
            margin: '0.75rem',
            background: 'var(--color-danger-bg)',
            color: 'var(--color-danger-fg)',
            border: '1px solid var(--color-danger-border)',
            borderRadius: 6,
            fontSize: '0.85rem',
          }}
        >
          <strong>
            Something went wrong rendering {this.props.fallbackLabel}.
          </strong>
          <p style={{ margin: '0.5rem 0', fontFamily: 'ui-monospace, monospace', fontSize: '0.78rem' }}>
            {this.state.error.message}
          </p>
          <button
            type="button"
            onClick={() => this.setState({ error: null })}
            style={{
              padding: '0.35rem 0.7rem',
              background: 'var(--color-danger-accent)',
              color: 'var(--color-text-on-accent)',
              border: 'none',
              borderRadius: 4,
              fontSize: '0.78rem',
              fontWeight: 600,
              cursor: 'pointer',
            }}
          >
            Try again
          </button>
        </div>
      )
    }
    return this.props.children
  }
}
