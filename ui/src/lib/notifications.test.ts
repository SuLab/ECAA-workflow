import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
  __resetBlinkForTests,
  blinkTitle,
  dismissPrompt,
  isPromptDismissed,
  notifyBlocker,
  permissionState,
  requestPermission,
} from './notifications'

// JSDOM doesn't define Notification by default. We install a mock
// constructor per test and restore afterwards.

class MockNotification {
  static permission: NotificationPermission = 'default'
  static lastInstance: MockNotification | null = null
  static requestPermission: () => Promise<NotificationPermission> = () =>
    Promise.resolve('granted')
  onclick: ((ev: unknown) => void) | null = null
  close = vi.fn()
  constructor(public title: string, public opts: NotificationOptions) {
    MockNotification.lastInstance = this
  }
}

declare global {
  interface Window {
    Notification: typeof MockNotification | undefined
  }
}

beforeEach(() => {
  window.localStorage.clear()
  MockNotification.permission = 'default'
  MockNotification.lastInstance = null
  MockNotification.requestPermission = vi
    .fn()
    .mockResolvedValue('granted') as () => Promise<NotificationPermission>
  // @ts-expect-error patching global for the test
  window.Notification = MockNotification
  // @ts-expect-error patching global for the test
  globalThis.Notification = MockNotification
  __resetBlinkForTests()
})

afterEach(() => {
  // @ts-expect-error cleanup
  delete window.Notification
  // @ts-expect-error cleanup
  delete globalThis.Notification
})

describe('permissionState', () => {
  it('reports granted/denied/default', () => {
    MockNotification.permission = 'granted'
    expect(permissionState()).toBe('granted')
    MockNotification.permission = 'denied'
    expect(permissionState()).toBe('denied')
    MockNotification.permission = 'default'
    expect(permissionState()).toBe('default')
  })

  it('reports unsupported when Notification is absent', () => {
    // @ts-expect-error removing
    delete globalThis.Notification
    expect(permissionState()).toBe('unsupported')
  })
})

describe('requestPermission', () => {
  it('returns current permission when already decided', async () => {
    MockNotification.permission = 'denied'
    const p = await requestPermission()
    expect(p).toBe('denied')
    expect(MockNotification.requestPermission).not.toHaveBeenCalled()
  })

  it('calls Notification.requestPermission when default', async () => {
    MockNotification.permission = 'default'
    const p = await requestPermission()
    expect(p).toBe('granted')
    expect(MockNotification.requestPermission).toHaveBeenCalled()
  })
})

describe('dismiss/prompt-dismissed', () => {
  it('round-trips localStorage', () => {
    expect(isPromptDismissed()).toBe(false)
    dismissPrompt()
    expect(isPromptDismissed()).toBe(true)
  })
})

describe('notifyBlocker', () => {
  it('fires a Notification when permission is granted', () => {
    MockNotification.permission = 'granted'
    notifyBlocker({ title: 'ping', body: 'body' })
    expect(MockNotification.lastInstance).not.toBeNull()
    expect(MockNotification.lastInstance!.title).toBe('ping')
  })

  it('does nothing (falls back to blink) when denied', () => {
    MockNotification.permission = 'denied'
    notifyBlocker({ title: 'ping', body: 'body' })
    expect(MockNotification.lastInstance).toBeNull()
  })
})

describe('blinkTitle', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    document.title = 'Scripps Workflow'
  })
  afterEach(() => {
    vi.useRealTimers()
    __resetBlinkForTests()
  })

  it('swaps document.title periodically', () => {
    blinkTitle()
    vi.advanceTimersByTime(1000)
    expect(document.title).toMatch(/⚠/)
    vi.advanceTimersByTime(1000)
    expect(document.title).toBe('Scripps Workflow')
  })
})
