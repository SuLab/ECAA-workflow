// opt-in after the first blocker lands. Tests stub window.Notification
// so the chip logic doesn't depend on the jsdom Notification support.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen } from '@testing-library/react'
import NotificationOptInChip from './NotificationOptInChip'

const originalNotification = (globalThis as { Notification?: unknown }).Notification

function stubNotification(permission: 'default' | 'granted' | 'denied') {
  class FakeNotification {
    static permission = permission
    static requestPermission = vi.fn().mockResolvedValue(permission)
  }
  Object.defineProperty(window, 'Notification', {
    configurable: true,
    writable: true,
    value: FakeNotification,
  })
}

function removeNotification() {
  Object.defineProperty(window, 'Notification', {
    configurable: true,
    writable: true,
    value: undefined,
  })
}

beforeEach(() => {
  window.localStorage.clear()
  stubNotification('default')
})

afterEach(() => {
  vi.restoreAllMocks()
  window.localStorage.clear()
  Object.defineProperty(window, 'Notification', {
    configurable: true,
    writable: true,
    value: originalNotification,
  })
})

describe('NotificationOptInChip', () => {
  it('renders nothing when everBlocked is false', () => {
    const { container } = render(<NotificationOptInChip everBlocked={false} />)
    expect(container.firstChild).toBeNull()
  })

  it('renders nothing when the Notification API is unsupported', () => {
    removeNotification()
    const { container } = render(<NotificationOptInChip everBlocked={true} />)
    expect(container.firstChild).toBeNull()
  })

  it('renders the Enable notifications button when everBlocked + default permission', () => {
    render(<NotificationOptInChip everBlocked={true} />)
    expect(screen.getByText('Enable notifications')).toBeInTheDocument()
  })

  it('clicking dismiss sets the localStorage key then hides the chip', () => {
    const { container } = render(<NotificationOptInChip everBlocked={true} />)
    fireEvent.click(screen.getByLabelText('Dismiss notification prompt'))
    expect(window.localStorage.getItem('swfc.notifications.promptDismissed')).toBe('1')
    expect(container.firstChild).toBeNull()
  })
})
