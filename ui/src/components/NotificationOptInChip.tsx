// Opt-in chip for browser notifications on blockers. Renders only when:
// - the browser supports Notifications
// - permission is still `default` (user hasn't chosen yet)
// - the SME hasn't dismissed the prompt previously
// - at least one blocker has already surfaced in the session (so we
// only ask after the SME has felt the pain).

import { useState } from 'react'
import {
  dismissPrompt,
  isPromptDismissed,
  permissionState,
  requestPermission,
} from '../lib/notifications'
import { useTitleBarPolling } from '../hooks/useTitleBarPolling'
import { NOTIF_PERMISSION_POLL_MS } from '../lib/polling'

interface Props {
  /** Has the session recorded at least one blocker previously? */
  everBlocked: boolean
}

export default function NotificationOptInChip({ everBlocked }: Props): JSX.Element | null {
  const [state, setState] = useState(() => permissionState())
  const [dismissed, setDismissed] = useState(() => isPromptDismissed())

  // Re-read permission in case another surface granted it. Joins the
  // shared title-bar tick (`useTitleBarPolling`) so we don't run a
  // dedicated 30s setInterval just for a one-line cache-bust.
  useTitleBarPolling({
    cadenceMs: NOTIF_PERMISSION_POLL_MS,
    enabled: true,
    onTick: () => setState(permissionState()),
  })

  if (!everBlocked) return null
  if (state === 'unsupported' || state === 'granted' || state === 'denied') return null
  if (dismissed) return null

  return (
    <span
      role="group"
      aria-label="Enable browser notifications"
      style={{
        display: 'inline-flex',
        alignItems: 'center',
        gap: '0.25rem',
        padding: '0.2rem 0.55rem',
        marginLeft: '0.5rem',
        background: 'var(--color-surface-1)',
        color: 'var(--color-text-secondary)',
        border: '1px solid var(--color-border-default)',
        borderRadius: 999,
        fontSize: '0.7rem',
      }}
    >
      <button
        type="button"
        onClick={async () => {
          const next = await requestPermission()
          setState(next)
        }}
        style={{
          background: 'transparent',
          border: 'none',
          padding: 0,
          cursor: 'pointer',
          color: 'var(--color-accent)',
          fontSize: '0.7rem',
          fontWeight: 600,
        }}
      >
        Enable notifications
      </button>
      <button
        type="button"
        aria-label="Dismiss notification prompt"
        onClick={() => {
          dismissPrompt()
          setDismissed(true)
        }}
        style={{
          background: 'transparent',
          border: 'none',
          padding: '0 0.1rem',
          cursor: 'pointer',
          color: 'var(--color-text-muted)',
          fontSize: '0.9rem',
          lineHeight: 1,
        }}
      >
        ×
      </button>
    </span>
  )
}
