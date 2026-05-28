// Consolidates the title-bar's per-chip polling onto a single
// `setInterval`. Pre-consolidation, four chips each opened their own
// timer (SessionTree 15s, BudgetChip 15s, RecentSessionsDropdown 30s
// when open, NotificationOptInChip 30s) → four wakeups per minute,
// each independently fanning out to its callback.
//
// This hook runs one 15s `setInterval` and broadcasts every tick to
// subscribed callbacks. Each subscriber declares its cadence in ms;
// the dispatcher only fires that subscriber when at least `cadenceMs`
// have elapsed since its last fire. A 30s subscriber therefore runs
// every other tick.
//
// Polling is suppressed while `document.visibilityState !== 'visible'`
// — the background-tab tax (silent state-drift catch-up the moment
// the tab regains focus) is paid via an immediate fire on the first
// `visible` tick after a hidden span.

import { useEffect, useRef } from 'react'

/** Master cadence — the smallest registered cadence rounded down. */
const TICK_MS = 15_000

type SubscriberCallback = () => void

interface Subscriber {
  cadenceMs: number
  callback: SubscriberCallback
  lastFireAt: number
}

// Module-level singleton: every consumer of `useTitleBarPolling`
// joins the same heartbeat. Lazily created on first subscriber, torn
// down when the last subscriber unmounts so unit tests / SSR don't
// keep a leaked timer alive.
let intervalId: number | null = null
let visibilityListenerInstalled = false
let lastTickAt = 0
const subscribers = new Set<Subscriber>()

function fireSubscriber(sub: Subscriber, now: number): void {
  try {
    sub.callback()
  } catch {
    // Subscribers MUST swallow their own errors; we still update
    // lastFireAt so a perma-failing subscriber doesn't get spammed.
  }
  sub.lastFireAt = now
}

function dispatch(now: number): void {
  if (typeof document !== 'undefined' && document.visibilityState !== 'visible') {
    return
  }
  for (const sub of subscribers) {
    if (now - sub.lastFireAt >= sub.cadenceMs) {
      fireSubscriber(sub, now)
    }
  }
}

function onVisibilityChange(): void {
  if (typeof document === 'undefined') return
  if (document.visibilityState === 'visible') {
    // Coming back from hidden — fire any subscriber that would have
    // fired during the hidden window so the SME sees fresh data
    // immediately on tab refocus.
    dispatch(Date.now())
  }
}

function ensureTimer(): void {
  if (intervalId !== null) return
  if (typeof window === 'undefined') return
  lastTickAt = Date.now()
  intervalId = window.setInterval(() => {
    lastTickAt = Date.now()
    dispatch(lastTickAt)
  }, TICK_MS)
  if (!visibilityListenerInstalled && typeof document !== 'undefined') {
    document.addEventListener('visibilitychange', onVisibilityChange)
    visibilityListenerInstalled = true
  }
}

function maybeTeardownTimer(): void {
  if (subscribers.size > 0) return
  if (intervalId !== null && typeof window !== 'undefined') {
    window.clearInterval(intervalId)
    intervalId = null
  }
  if (visibilityListenerInstalled && typeof document !== 'undefined') {
    document.removeEventListener('visibilitychange', onVisibilityChange)
    visibilityListenerInstalled = false
  }
}

interface UseTitleBarPollingOptions {
  /** Effective poll cadence in ms (e.g. 15_000 for SessionTree). */
  cadenceMs: number
  /**
   * When false, the subscriber is unregistered for the duration —
   * `RecentSessionsDropdown` only polls when its panel is open, so
   * passing `enabled={open}` keeps the timer asleep otherwise.
   */
  enabled: boolean
  /**
   * Stable callback. The hook stores it via `useRef` so callers can
   * write a fresh inline closure each render without re-registering
   * the subscription.
   */
  onTick: SubscriberCallback
}

/**
 * Register a chip's polling callback on the shared title-bar tick.
 *
 * Subscribers receive `onTick` invocations approximately every
 * `cadenceMs` while the tab is visible. The first invocation does
 * NOT fire on mount — callers run their initial fetch via their own
 * `useEffect` (so callers control mount-time error / loading paths
 * directly). The tick is purely for *subsequent* refreshes.
 */
export function useTitleBarPolling({
  cadenceMs,
  enabled,
  onTick,
}: UseTitleBarPollingOptions): void {
  const callbackRef = useRef(onTick)
  callbackRef.current = onTick

  useEffect(() => {
    if (!enabled) return
    const sub: Subscriber = {
      cadenceMs,
      callback: () => callbackRef.current(),
      // Initialize lastFireAt to `now` so the FIRST scheduled fire is
      // approximately `cadenceMs` after registration — preserves the
      // pre-consolidation semantics of `setInterval(cb, cadenceMs)`,
      // which also defers the first fire by `cadenceMs`.
      lastFireAt: Date.now(),
    }
    subscribers.add(sub)
    ensureTimer()
    return () => {
      subscribers.delete(sub)
      maybeTeardownTimer()
    }
  }, [cadenceMs, enabled])
}

// Test-only escape hatch. Vitest setup hooks call this to force a
// clean module state between cases. Not part of the public API.
export function __resetTitleBarPollingForTest(): void {
  for (const s of Array.from(subscribers)) subscribers.delete(s)
  maybeTeardownTimer()
  lastTickAt = 0
}
