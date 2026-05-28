// In-titlebar display for the current session's auto-generated short
// name plus an "Auto-name" button that triggers the Haiku-powered side
// call.
//
// Rendering contract:
// - When `session.title` is set: render the title text; no button.
// - When `session.title` is null AND the feature flag is on AND the
// session has enough turns: render an "Auto-name" button that POSTs
// to /api/chat/session/:id/auto-title, then triggers the supplied
// `onTitled` callback so the caller can refresh its state.
// - Otherwise: render nothing (graceful degradation — old server, off
// flag, or too-short session).
//
// The component fetches /api/chat/config once on mount (cached in a
// module-level memo so every rerender doesn't re-probe the server).

import { useMemo, useState } from 'react'
import {
  autoTitleSession,
  getChatConfig,
  type ChatConfig,
} from '../api/chatClient'
import { useCancelableEffect } from '../hooks/useCancelableFetch'

interface Props {
  sessionId: string | null
  title: string | null | undefined
  turnCount: number
  onTitled: (title: string) => void
}

// Cache the config lookup so we probe the server once per page load —
// the feature flag is set at server startup and can't change mid-
// session, so a longer-lived cache is fine.
let cachedConfigPromise: Promise<ChatConfig> | null = null
function configOnce(): Promise<ChatConfig> {
  if (!cachedConfigPromise) cachedConfigPromise = getChatConfig()
  return cachedConfigPromise
}

// Exported for test setup/teardown so Vitest can start each test from
// a clean cache — otherwise the fake fetch in one test leaks into the
// next.
export function __resetSessionTitleBarCache(): void {
  cachedConfigPromise = null
}

type ButtonState =
  | { kind: 'idle' }
  | { kind: 'running' }
  | { kind: 'error'; message: string }

export default function SessionTitleBar({
  sessionId,
  title,
  turnCount,
  onTitled,
}: Props): JSX.Element | null {
  const [config, setConfig] = useState<ChatConfig | null>(null)
  const [button, setButton] = useState<ButtonState>({ kind: 'idle' })

  useCancelableEffect(async ({ cancelled }) => {
    try {
      const c = await configOnce()
      if (!cancelled()) setConfig(c)
    } catch {
      // Server too old to expose /api/chat/config — treat as flag-off.
      if (!cancelled()) setConfig({ auto_title_enabled: false, auto_title_min_turns: 3 })
    }
  }, [])

  const canShowButton = useMemo(() => {
    if (!sessionId) return false
    if (title) return false
    if (!config?.auto_title_enabled) return false
    return turnCount >= config.auto_title_min_turns
  }, [sessionId, title, config, turnCount])

  if (!sessionId) return null

  // Title present → render just the text. No button.
  if (title) {
    return (
      <span
        data-session-title="present"
        style={{
          color: 'var(--color-chrome-fg-muted)',
          fontSize: '0.78rem',
          fontStyle: 'italic',
          maxWidth: '24rem',
          overflow: 'hidden',
          textOverflow: 'ellipsis',
          whiteSpace: 'nowrap',
        }}
        title={title}
      >
        {title}
      </span>
    )
  }

  if (!canShowButton) return null

  const running = button.kind === 'running'
  const onClick = async () => {
    if (!sessionId || running) return
    setButton({ kind: 'running' })
    try {
      const resp = await autoTitleSession(sessionId)
      onTitled(resp.title)
      setButton({ kind: 'idle' })
    } catch (e) {
      setButton({
        kind: 'error',
        message: e instanceof Error ? e.message : String(e),
      })
    }
  }

  return (
    <span style={{ display: 'inline-flex', gap: '0.35rem', alignItems: 'center' }}>
      <button
        type="button"
        onClick={onClick}
        disabled={running}
        data-session-title="button"
        aria-label="Auto-name this session with Haiku"
        style={{
          padding: '0.2rem 0.55rem',
          fontSize: '0.7rem',
          fontWeight: 500,
          background: running
            ? 'var(--color-chrome-border-strong)'
            : 'var(--color-chrome-bg-elevated)',
          color: running
            ? 'var(--color-chrome-fg-faint)'
            : 'var(--color-chrome-fg-muted)',
          border: '1px solid var(--color-chrome-border-strong)',
          borderRadius: 999,
          cursor: running ? 'wait' : 'pointer',
          fontFamily: 'inherit',
        }}
      >
        {running ? 'Naming…' : 'Auto-name'}
      </button>
      {button.kind === 'error' && (
        <span
          role="alert"
          style={{ color: 'var(--color-danger-accent)', fontSize: '0.68rem' }}
          title={button.message}
        >
          failed
        </span>
      )}
    </span>
  )
}
