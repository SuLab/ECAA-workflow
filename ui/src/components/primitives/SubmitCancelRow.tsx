import type { CardPalette } from '../../styles/palettes'
import { CARD_PALETTES } from '../../styles/palettes'

interface SubmitCancelRowProps {
  onSubmit: () => void | Promise<void>
  onCancel?: () => void
  submitLabel?: string
  cancelLabel?: string
  busy?: boolean
  submitDisabled?: boolean
  /**
   * Disables BOTH buttons without changing the submit label (unlike
   * `busy`, which both disables and swaps the label to "Working…").
   * Use when the enclosing card is in a non-interactive terminal state.
   */
  disabled?: boolean
  /** Matches the parent card's palette so the primary button color is consistent. */
  palette?: CardPalette
}

/**
 * Shared "primary + secondary" button row used by every interactive
 * card. The submit button picks its background from the parent's
 * palette; the cancel button is always transparent with a subtle
 * border.
 */
export function SubmitCancelRow({
  onSubmit,
  onCancel,
  submitLabel = 'Submit',
  cancelLabel = 'Cancel',
  busy,
  submitDisabled,
  disabled,
  palette = 'warning',
}: SubmitCancelRowProps): JSX.Element {
  const p = CARD_PALETTES[palette]
  const submitOff = busy || submitDisabled || disabled
  const cancelOff = busy || disabled
  return (
    <div style={{ display: 'flex', gap: '0.5rem' }}>
      <button
        type="button"
        onClick={() => {
          void onSubmit()
        }}
        disabled={submitOff}
        style={{
          padding: '0.45rem 0.9rem',
          background: p.accent,
          color: 'var(--color-text-on-accent)',
          border: 'none',
          borderRadius: 6,
          cursor: submitOff ? 'not-allowed' : 'pointer',
          fontSize: '0.8rem',
          fontWeight: 600,
          opacity: submitOff ? 0.6 : 1,
        }}
      >
        {busy ? 'Working…' : submitLabel}
      </button>
      {onCancel && (
        <button
          type="button"
          onClick={onCancel}
          disabled={cancelOff}
          style={{
            padding: '0.45rem 0.9rem',
            background: 'transparent',
            color: p.fg,
            border: `1px solid ${p.border}`,
            borderRadius: 6,
            cursor: cancelOff ? 'not-allowed' : 'pointer',
            fontSize: '0.8rem',
            fontWeight: 500,
            opacity: cancelOff ? 0.6 : 1,
          }}
        >
          {cancelLabel}
        </button>
      )}
    </div>
  )
}
