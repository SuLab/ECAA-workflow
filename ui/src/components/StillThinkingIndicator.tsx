import type { ThinkingStage } from '../hooks/useConversation'
import { CardContainer } from './primitives/CardContainer'

/**
 * Whole-turn "Still thinking…" indicator with progressive escalation
 * (D8 mitigation). Distinct from the 1s `ToolCallStatusPill` that
 * fires per individual tool call — this one is driven by
 * `useConversation.thinkingStage` and escalates through four messages
 * matching the 8s / 30s / 60s / 90s thresholds defined in
 * `lib/polling.ts`:
 *
 * - `still_working` (>=8s) — gentle reassurance.
 * - `slow` (>=30s) — explicit "Anthropic is slow."
 * - `very_slow` (>=60s) — "If this is stuck, you can refresh."
 * - `cancelable` (>=90s) — Cancel button visible.
 *
 * `idle` / `thinking` render nothing (caller already gates with
 * `stillThinking` for back-compat). The server-side
 * `ECAA_ANTHROPIC_TIMEOUT_SECS=180` ceiling will surface a typed
 * `Backend` error if the call really hung; the chip's Cancel just
 * releases the UI's busy gate so the SME doesn't have to wait the
 * remaining 90s.
 */
export default function StillThinkingIndicator(props: {
  stage?: ThinkingStage
  onCancel?: () => void
}) {
  const stage = props.stage ?? 'still_working'
  const { message, palette, dotColor } = messageForStage(stage)
  const showCancel = stage === 'cancelable' && typeof props.onCancel === 'function'
  return (
    <CardContainer
      palette={palette}
      role="status"
      ariaLive="polite"
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: '0.55rem',
        padding: '0.45rem 0.85rem',
        margin: '0.5rem 0.75rem',
        fontSize: '0.78rem',
        color: `var(--color-${palette}-fg)`,
        borderLeft: `1px solid var(--color-${palette}-border)`,
      }}
    >
      <span
        aria-hidden="true"
        style={{
          width: 10,
          height: 10,
          borderRadius: '50%',
          background: dotColor,
          animation: 'ecaaPulse 1.4s ease-in-out infinite',
          flexShrink: 0,
        }}
      />
      <span style={{ flex: 1 }}>{message}</span>
      {showCancel && (
        <button
          type="button"
          onClick={props.onCancel}
          aria-label="Cancel the in-flight request"
          style={{
            border: '1px solid var(--color-warning-border)',
            background: 'transparent',
            color: 'var(--color-warning-fg)',
            padding: '0.2rem 0.6rem',
            borderRadius: 3,
            fontSize: '0.74rem',
            cursor: 'pointer',
          }}
        >
          Cancel
        </button>
      )}
    </CardContainer>
  )
}

function messageForStage(stage: ThinkingStage): {
  message: string
  palette: 'warning' | 'danger'
  dotColor: string
} {
  switch (stage) {
    case 'slow':
      return {
        message:
          "Still working — Anthropic is slow right now. This usually clears on its own.",
        palette: 'warning',
        dotColor: 'var(--color-warning-accent)',
      }
    case 'very_slow':
      return {
        message:
          'This is taking longer than usual. If it seems stuck, you can refresh the page.',
        palette: 'warning',
        dotColor: 'var(--color-warning-accent)',
      }
    case 'cancelable':
      return {
        message:
          "The request hasn't responded in 90 seconds. Cancel to retry or send a different message.",
        palette: 'danger',
        dotColor: 'var(--color-danger-accent)',
      }
    case 'still_working':
    default:
      return {
        message:
          "Still thinking — this can take a few seconds when there's a lot to work through.",
        palette: 'warning',
        dotColor: 'var(--color-warning-accent)',
      }
  }
}
