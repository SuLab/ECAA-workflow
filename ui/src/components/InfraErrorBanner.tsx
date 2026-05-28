import type { InfraError } from '../hooks/useSseChatEvents'
import ExplainButton from './ExplainButton'
import { CardContainer } from './primitives/CardContainer'

interface Props {
  error: InfraError
  onDismiss?: () => void
  /** Session id so the ExplainButton can bill its Haiku side-call. */
  sessionId?: string | null
  /** Optional retry handler — when provided, surfaces a "Retry last turn"
   *  button that the App-level wiring resends the most recent user turn
   *  through. (S5.11) */
  retry?: () => void
}

const COPY: Record<string, { title: string; body: string }> = {
  api_unreachable: {
    title: 'The assistant is temporarily unavailable',
    body: 'Your conversation is saved. Please try again in a moment.',
  },
  session_lost: {
    title: 'Session could not be restored',
    body: 'Please refresh the page and start a new conversation.',
  },
  retry_exhausted: {
    title: 'I had trouble with that last step',
    body: 'The conversation is paused. Try rephrasing your last message.',
  },
}

export default function InfraErrorBanner({
  error,
  onDismiss,
  sessionId,
  retry,
}: Props) {
  const fallback = {
    title: 'Something went wrong',
    body: error.userCopy || 'Please try again.',
  }
  const copy = COPY[error.reason] ?? fallback
  return (
    <CardContainer
      palette="danger"
      role="alert"
      ariaLive="assertive"
      title={copy.title}
      style={{
        padding: '0.75rem 1rem',
        color: 'var(--color-danger-fg)',
        fontSize: '0.85rem',
        margin: '0.5rem 0.75rem',
        // No accent left-border for the banner variant — match prior chrome.
        borderLeft: '1px solid var(--color-danger-border)',
      }}
    >
      <span>{copy.body}</span>
      <ExplainButton
        text={copy.body}
        context="infrastructure error banner"
        sessionIdOverride={sessionId ?? null}
      />
      {retry && (
        <button
          type="button"
          onClick={retry}
          aria-label="Retry last turn"
          style={{
            marginLeft: 8,
            background: 'transparent',
            border: '1px solid var(--color-danger-border)',
            borderRadius: 4,
            color: 'var(--color-danger-fg)',
            cursor: 'pointer',
            fontSize: '0.78rem',
            padding: '0.15rem 0.5rem',
          }}
        >
          Retry last turn
        </button>
      )}
      {onDismiss && (
        <button
          type="button"
          onClick={onDismiss}
          aria-label="Dismiss notification"
          style={{
            marginLeft: 8,
            background: 'transparent',
            border: 'none',
            color: 'var(--color-danger-fg)',
            cursor: 'pointer',
            fontSize: '0.78rem',
            textDecoration: 'underline',
          }}
        >
          Dismiss
        </button>
      )}
    </CardContainer>
  )
}
