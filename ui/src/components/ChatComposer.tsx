import { useEffect, useRef, useState } from 'react'

interface Props {
  onSend: (text: string) => void | Promise<void>
  disabled?: boolean
  placeholder?: string
  /// Auto-focus the textarea on mount so a keyboard-first SME can start
  /// typing immediately. Closes a known a11y-audit follow-up item.
  autoFocus?: boolean
}

export default function ChatComposer({
  onSend,
  disabled,
  placeholder = 'Describe what you want to do…',
  autoFocus = true,
}: Props) {
  const [text, setText] = useState('')
  const textareaRef = useRef<HTMLTextAreaElement>(null)

  useEffect(() => {
    if (autoFocus && textareaRef.current && !disabled) {
      textareaRef.current.focus()
    }
  }, [autoFocus, disabled])

  const submit = async () => {
    const trimmed = text.trim()
    if (!trimmed || disabled) return
    // Defer clearing the textarea until `onSend` resolves: a transient
    // network failure (offline, server restart) shouldn't lose the
    // SME's typed message. The textarea stays populated so they can
    // retry without re-typing.
    try {
      await onSend(trimmed)
      setText('')
    } catch {
      // Preserve `text` so the SME can edit + resend. The caller is
      // responsible for surfacing the failure (toast / banner /
      // session-status pill); we just don't drop the draft.
    }
  }

  return (
    <div
      style={{
        display: 'flex',
        gap: '0.5rem',
        padding: '0.65rem 0.75rem',
        borderTop: '1px solid var(--color-border-default)',
        background: 'var(--color-surface-1)',
        flexShrink: 0,
      }}
    >
      <textarea
        ref={textareaRef}
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && !e.shiftKey) {
            e.preventDefault()
            void submit()
          }
        }}
        placeholder={placeholder}
        rows={2}
        disabled={disabled}
        aria-label="Message"
        style={{
          flex: 1,
          resize: 'none',
          padding: '0.5rem 0.6rem',
          border: '1px solid var(--color-border-strong)',
          borderRadius: 6,
          fontSize: '0.85rem',
          fontFamily: 'inherit',
          outline: 'none',
          background: disabled ? 'var(--color-surface-0)' : 'var(--color-surface-1)',
          color: 'var(--color-text-primary)',
        }}
      />
      <button
        type="button"
        onClick={() => void submit()}
        disabled={disabled || !text.trim()}
        aria-label="Send message"
        style={{
          padding: '0 1rem',
          background: 'var(--color-button-primary-bg)',
          color: 'var(--color-button-primary-fg)',
          border: 'none',
          borderRadius: 6,
          cursor: disabled || !text.trim() ? 'not-allowed' : 'pointer',
          fontSize: '0.83rem',
          fontWeight: 600,
          opacity: disabled || !text.trim() ? 0.4 : 1,
          flexShrink: 0,
        }}
      >
        Send
      </button>
    </div>
  )
}
