// Browser Notifications API shim for blocker surfacing.
//
// Wraps permission request, notification firing, and title-bar blink
// fallback for browsers without support. Callers:
// - useSseChatEvents fires notifyBlocker when state_advanced→blocked
// and document.visibilityState === 'hidden'.
// - An opt-in chip in the title bar drives requestPermission; the
// dismissal is persisted in localStorage under
// swfc.notifications.promptDismissed so we don't keep nagging.

import { TITLE_BLINK_INTERVAL_MS } from './polling'

const PROMPT_DISMISSED_KEY = 'swfc.notifications.promptDismissed'
const BLINK_TITLE = '⚠ ECAA-workflow'

export type NotificationPermissionState =
  | 'default'
  | 'granted'
  | 'denied'
  | 'unsupported'

/** Read-only: returns the current browser Notification permission. */
export function permissionState(): NotificationPermissionState {
  if (typeof window === 'undefined' || typeof Notification === 'undefined') {
    return 'unsupported'
  }
  return Notification.permission as NotificationPermissionState
}

/** Returns true when the SME previously clicked Dismiss on the opt-in chip. */
export function isPromptDismissed(): boolean {
  try {
    return window.localStorage.getItem(PROMPT_DISMISSED_KEY) === '1'
  } catch {
    return false
  }
}

export function dismissPrompt(): void {
  try {
    window.localStorage.setItem(PROMPT_DISMISSED_KEY, '1')
  } catch {
    // ignore (private browsing / disabled storage)
  }
}

/** Ask the browser for notification permission. Returns final state. */
export async function requestPermission(): Promise<NotificationPermissionState> {
  if (typeof Notification === 'undefined') return 'unsupported'
  if (Notification.permission !== 'default') {
    return Notification.permission as NotificationPermissionState
  }
  try {
    const result = await Notification.requestPermission()
    return result as NotificationPermissionState
  } catch {
    return 'denied'
  }
}

interface BlockerPayload {
  title: string
  body: string
  taskId?: string
}

/**
 * Fire a blocker notification. When the browser doesn't support
 * Notifications or permission is denied, fall back to the title-bar
 * blink (driven by `blinkTitle`). Click handler focuses the tab and
 * deep-links into the blocked task via URL hash.
 */
export function notifyBlocker(payload: BlockerPayload): void {
  const state = permissionState()
  if (state !== 'granted') {
    blinkTitle()
    return
  }
  try {
    const n = new Notification(payload.title, {
      body: payload.body,
      requireInteraction: true,
      tag: payload.taskId ? `blocker-${payload.taskId}` : 'blocker',
    })
    n.onclick = () => {
      try {
        window.focus()
        if (payload.taskId) {
          window.location.hash = `task=${encodeURIComponent(payload.taskId)}`
        }
      } finally {
        n.close()
      }
    }
  } catch {
    blinkTitle()
  }
}

let blinkInterval: number | null = null
let originalTitle: string | null = null

/**
 * Swap the document title between a warning marker and the real title
 * every 1000ms. Fallback for browsers with no Notifications support
 * or denied permission. Automatically stops on the next visibility
 * change (the SME has refocused the tab).
 */
export function blinkTitle(): void {
  if (typeof document === 'undefined') return
  if (blinkInterval !== null) return
  originalTitle = document.title
  let showWarning = true
  blinkInterval = window.setInterval(() => {
    document.title = showWarning ? BLINK_TITLE : originalTitle ?? 'ECAA-workflow'
    showWarning = !showWarning
  }, TITLE_BLINK_INTERVAL_MS)
  const stop = () => {
    if (blinkInterval !== null) {
      window.clearInterval(blinkInterval)
      blinkInterval = null
    }
    if (originalTitle !== null) {
      document.title = originalTitle
      originalTitle = null
    }
    document.removeEventListener('visibilitychange', stop)
  }
  document.addEventListener('visibilitychange', stop)
}

/** Unit-test hook: reset the title blink state. */
export function __resetBlinkForTests(): void {
  if (blinkInterval !== null) {
    window.clearInterval(blinkInterval)
    blinkInterval = null
  }
  originalTitle = null
}
