// Share modal. Generates a read-only URL, copies it, and lets the
// SME revoke prior tokens.

import { useEffect, useState } from 'react'
import {
  createShareToken,
  listShareTokens,
  revokeShareToken,
  type ShareTokenDescriptor,
} from '../api/chatClient'
import { relativeTime } from '../lib/time'
import { Dialog } from './primitives/Dialog'

interface Props {
  sessionId: string | null
  onClose: () => void
}

const PRESETS: Array<{ label: string; hours: number | null }> = [
  { label: '24 hours', hours: 24 },
  { label: '7 days', hours: 24 * 7 },
  { label: 'Never', hours: null },
]

export default function ShareModal({ sessionId, onClose }: Props): JSX.Element | null {
  const [tokens, setTokens] = useState<ShareTokenDescriptor[]>([])
  const [preset, setPreset] = useState<number | null>(24 * 7)
  const [error, setError] = useState<string | null>(null)
  const [creating, setCreating] = useState(false)
  const [copyFeedback, setCopyFeedback] = useState<string | null>(null)
  const [lastUrl, setLastUrl] = useState<string | null>(null)
  const [disabled, setDisabled] = useState(false)

  // Focus management + Escape + outside-click are now handled by the
  // shared Dialog primitive — drops ~25 LOC of bespoke wiring.

  const refresh = async () => {
    if (!sessionId) return
    try {
      const list = await listShareTokens(sessionId)
      setTokens(list)
      setError(null)
      setDisabled(false)
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e)
      if (msg.includes('503')) {
        setDisabled(true)
      } else {
        setError(msg)
      }
    }
  }

  useEffect(() => {
    void refresh()
  // eslint-disable-next-line react-hooks/exhaustive-deps -- refresh is an inline async fn; adding it recreates on every render causing an infinite loop
  }, [sessionId])

  const createAndCopy = async () => {
    if (!sessionId) return
    setCreating(true)
    setError(null)
    try {
      const t = await createShareToken(sessionId, preset)
      const url = buildShareUrl(sessionId, t.token)
      setLastUrl(url)
      try {
        await navigator.clipboard.writeText(url)
        setCopyFeedback('URL copied to clipboard.')
      } catch {
        setCopyFeedback(`Share URL ready below.`)
      }
      await refresh()
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setCreating(false)
    }
  }

  const revoke = async (token: string) => {
    if (!sessionId) return
    try {
      await revokeShareToken(sessionId, token)
      await refresh()
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }

  if (!sessionId) return null

  return (
    <Dialog
      onClose={onClose}
      ariaLabel="Share session"
      contentStyle={{ width: 520, maxWidth: '92vw' }}
    >
        <div style={{ display: 'flex', alignItems: 'center', marginBottom: '0.7rem' }}>
          <h3 style={{ margin: 0, fontSize: '1rem' }}>Share session (read-only)</h3>
          <div style={{ flex: 1 }} />
          <button
            type="button"
            aria-label="Close"
            onClick={onClose}
            style={{
              background: 'transparent',
              border: 'none',
              fontSize: '1.2rem',
              cursor: 'pointer',
              color: 'var(--color-text-muted)',
            }}
          >
            ×
          </button>
        </div>
        {disabled && (
          <div
            role="alert"
            style={{ color: 'var(--color-warning-accent)', fontSize: '0.85rem' }}
          >
            Shared URLs are disabled. Ask an operator to set
            ECAA_SHARED_URLS_ENABLED=1 on the server.
          </div>
        )}
        {error && (
          <div
            role="alert"
            style={{ color: 'var(--color-danger-accent)', fontSize: '0.85rem' }}
          >
            {error}
          </div>
        )}
        {!disabled && (
          <>
            <div style={{ marginTop: '0.4rem', fontSize: '0.85rem' }}>
              Generate a URL anyone can open to see this session without
              being able to change anything.
            </div>
            <div style={{ marginTop: '0.7rem', display: 'flex', gap: '0.5rem' }}>
              {PRESETS.map((p) => (
                <button
                  key={p.label}
                  type="button"
                  aria-pressed={preset === p.hours}
                  onClick={() => setPreset(p.hours)}
                  style={chipStyle(preset === p.hours)}
                >
                  {p.label}
                </button>
              ))}
              <button
                type="button"
                disabled={creating}
                onClick={() => void createAndCopy()}
                style={{
                  marginLeft: 'auto',
                  padding: '0.35rem 0.8rem',
                  border: '1px solid var(--color-accent)',
                  background: 'var(--color-accent)',
                  color: 'var(--color-accent-fg)',
                  borderRadius: 4,
                  fontSize: '0.82rem',
                  fontWeight: 600,
                  cursor: creating ? 'progress' : 'pointer',
                }}
              >
                {creating ? 'Creating…' : 'Create link'}
              </button>
            </div>
            {copyFeedback && (
              <div
                role="status"
                style={{
                  marginTop: '0.5rem',
                  fontSize: '0.8rem',
                  color: 'var(--color-success-accent)',
                }}
              >
                {copyFeedback}
              </div>
            )}
            {lastUrl && (
              <input
                readOnly
                aria-label="Share URL"
                value={lastUrl}
                onFocus={(e) => e.currentTarget.select()}
                style={{
                  marginTop: '0.5rem',
                  width: '100%',
                  padding: '0.4rem 0.5rem',
                  fontSize: '0.78rem',
                  fontFamily: 'ui-monospace, monospace',
                  border: '1px solid var(--color-border-default)',
                  borderRadius: 4,
                  background: 'var(--color-surface-1)',
                  color: 'var(--color-text-primary)',
                }}
              />
            )}
            <div style={{ marginTop: '1rem' }}>
              <h4 style={{ margin: '0 0 0.4rem', fontSize: '0.8rem' }}>
                Active tokens
              </h4>
              {tokens.length === 0 ? (
                <div style={{ fontSize: '0.8rem', color: 'var(--color-text-muted)' }}>
                  No active share tokens.
                </div>
              ) : (
                <ul style={{ listStyle: 'none', margin: 0, padding: 0 }}>
                  {tokens.map((t) => (
                    <li
                      key={t.token}
                      style={{
                        padding: '0.45rem 0',
                        borderTop: '1px solid var(--color-border-subtle)',
                        display: 'flex',
                        alignItems: 'center',
                        gap: '0.5rem',
                        fontSize: '0.8rem',
                      }}
                    >
                      <code style={{ color: 'var(--color-text-primary)' }}>
                        …{t.token.slice(-8)}
                      </code>
                      <span style={{ color: 'var(--color-text-muted)' }}>
                        {t.expires_at
                          ? `expires ${relativeTime(t.expires_at)}`
                          : 'no expiry'}
                      </span>
                      <div style={{ flex: 1 }} />
                      <button
                        type="button"
                        onClick={() => void revoke(t.token)}
                        style={{
                          padding: '0.2rem 0.55rem',
                          fontSize: '0.75rem',
                          border: '1px solid var(--color-border-default)',
                          background: 'var(--color-surface-1)',
                          color: 'var(--color-text-secondary)',
                          borderRadius: 4,
                          cursor: 'pointer',
                        }}
                      >
                        Revoke
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          </>
        )}
    </Dialog>
  )
}

function buildShareUrl(sessionId: string, token: string): string {
  // Share URL shape: current origin + ?session=<id>&share_token=<token>
  const { origin } = window.location
  const params = new URLSearchParams()
  params.set('session', sessionId)
  params.set('share_token', token)
  return `${origin}/?${params.toString()}`
}

function chipStyle(active: boolean): React.CSSProperties {
  return {
    padding: '0.25rem 0.7rem',
    fontSize: '0.78rem',
    borderRadius: 999,
    border: active
      ? '1px solid var(--color-accent)'
      : '1px solid var(--color-border-default)',
    background: active ? 'var(--color-accent)' : 'var(--color-surface-1)',
    color: active ? 'var(--color-accent-fg)' : 'var(--color-text-secondary)',
    cursor: 'pointer',
  }
}
