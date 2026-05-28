/**
 * Surfaces filesystem paths the server's intake-prose path-hint
 * extractor pulled out of SME prose but the SME hasn't yet
 * registered as session inputs. Renders one row per hint with a
 * one-click "Register" button that posts to `/inputs/path`.
 *
 * This is the UI half of e2e fix #13: before this component,
 * SMEs who typed "the CSV is at /home/me/data/foo.csv" had to
 * separately open the Inputs tab and re-paste the directory path
 * to register it; the execution agent then fabricated synthetic
 * data because it found `runtime/inputs.json` empty. With this
 * card, the path the SME mentioned is one click away from being
 * registered, and the chat assistant is prompted (see
 * `prompt_role.txt`) to surface the same suggestion in plain
 * language alongside.
 *
 * Hints disappear from the array once the server's
 * `extract_and_apply_path_hints` sees the corresponding
 * `canonical_root` in `session.inputs`, so the SME never has to
 * dismiss them explicitly.
 */

import { useState } from 'react'
import type { InputPathHint } from '../types/InputPathHint'
import { registerInputPath } from '../api/chatClient'

interface Props {
  sessionId: string
  hints: InputPathHint[]
  /** Called after a successful registration so the parent can refresh state. */
  onRegistered?: () => void | Promise<void>
}

export default function PendingInputHintsCard({
  sessionId,
  hints,
  onRegistered,
}: Props): JSX.Element | null {
  // Per-hint pending/error state so the user can re-try a failed
  // registration without blocking the others. Keyed on
  // `canonical_root` since that's what gets POSTed.
  const [pending, setPending] = useState<Record<string, boolean>>({})
  const [errors, setErrors] = useState<Record<string, string>>({})

  if (!hints || hints.length === 0) return null

  const handleRegister = async (hint: InputPathHint): Promise<void> => {
    const key = hint.canonical_root
    setPending((p) => ({ ...p, [key]: true }))
    setErrors((e) => {
      const { [key]: _, ...rest } = e
      return rest
    })
    try {
      // `label` defaults server-side to the directory basename when
      // omitted. We pass the verbatim mention so the SME sees the
      // same string they typed in the Inputs tab listing.
      const label = hint.file_relpath ?? undefined
      await registerInputPath(sessionId, {
        path: hint.canonical_root,
        label,
      })
      if (onRegistered) {
        await onRegistered()
      }
    } catch (err) {
      const msg =
        err instanceof Error ? err.message : String(err ?? 'registration failed')
      setErrors((e) => ({ ...e, [key]: msg }))
    } finally {
      setPending((p) => {
        const { [key]: _, ...rest } = p
        return rest
      })
    }
  }

  return (
    <section
      aria-label="Detected data inputs"
      style={{
        margin: '0.5rem 0.75rem',
        padding: '0.65rem 0.85rem',
        border: '1px solid var(--color-info-border, #2e4a6e)',
        background: 'var(--color-info-bg, #1a2638)',
        color: 'var(--color-text-primary, #d6e2f0)',
        borderRadius: 6,
        fontSize: '0.82rem',
      }}
    >
      <div style={{ fontWeight: 600, marginBottom: '0.45rem' }}>
        Detected data path{hints.length > 1 ? 's' : ''} in your message
      </div>
      <div style={{ fontSize: '0.72rem', opacity: 0.78, marginBottom: '0.55rem' }}>
        Click <em>Register</em> to make {hints.length > 1 ? 'these' : 'this'} available to the execution agent.
        The Inputs tab in the right pane shows registered sources after.
      </div>
      <ul
        style={{
          listStyle: 'none',
          padding: 0,
          margin: 0,
          display: 'flex',
          flexDirection: 'column',
          gap: '0.4rem',
        }}
      >
        {hints.map((h) => {
          const key = h.canonical_root
          const isPending = pending[key] === true
          const error = errors[key]
          return (
            <li
              key={`${key}|${h.file_relpath ?? ''}`}
              style={{
                display: 'flex',
                alignItems: 'center',
                gap: '0.6rem',
                padding: '0.35rem 0.5rem',
                background: 'var(--color-surface, #0f1827)',
                borderRadius: 4,
              }}
            >
              <div style={{ flex: 1, minWidth: 0 }}>
                <div
                  style={{
                    fontFamily: 'var(--font-mono, monospace)',
                    fontSize: '0.74rem',
                    overflow: 'hidden',
                    textOverflow: 'ellipsis',
                    whiteSpace: 'nowrap',
                  }}
                  title={h.raw_mention}
                >
                  {h.raw_mention}
                </div>
                <div
                  style={{
                    fontSize: '0.66rem',
                    opacity: 0.65,
                    marginTop: '0.15rem',
                  }}
                >
                  {h.file_mention
                    ? `file → registers parent ${h.canonical_root}`
                    : `directory ${h.canonical_root}`}
                </div>
                {error !== undefined && (
                  <div
                    role="alert"
                    style={{
                      fontSize: '0.68rem',
                      color: 'var(--color-error-fg, #f4a4a4)',
                      marginTop: '0.2rem',
                    }}
                  >
                    {error}
                  </div>
                )}
              </div>
              <button
                type="button"
                aria-label={`Register data path ${h.canonical_root}`}
                onClick={() => {
                  void handleRegister(h)
                }}
                disabled={isPending}
                style={{
                  background: isPending
                    ? 'var(--color-disabled-bg, #2a3b52)'
                    : 'var(--color-action-bg, #2563eb)',
                  color: 'var(--color-action-fg, #ffffff)',
                  border: 'none',
                  padding: '0.3rem 0.65rem',
                  borderRadius: 4,
                  fontSize: '0.72rem',
                  cursor: isPending ? 'wait' : 'pointer',
                  fontWeight: 600,
                }}
              >
                {isPending ? 'Registering…' : 'Register'}
              </button>
            </li>
          )
        })}
      </ul>
    </section>
  )
}
