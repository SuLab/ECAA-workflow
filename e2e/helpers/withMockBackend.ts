/**
 * Tiny wrapper around
 * `installMockBackend` that collapses the `try {... } finally {
 * handle.dispose() }` pair scattered across 40+ test bodies.
 *
 * Usage:
 *  await withMockBackend(page, beats, async (handle) => {
 *  await page.goto('/')
 *  //... test body...
 *  Await handle.pushSseEvent({ type: 'state_advanced',... })
 *  })
 *
 * The ~3 tests that need a bespoke lifecycle (long-lived handles,
 * mid-test re-install) should continue to call `installMockBackend`
 * directly.
 */

import type { Page } from '@playwright/test'
import { installMockBackend } from './mockBackend'
import type { MockBackendHandle, MockBackendOptions } from './mockBackend'
import type { Beat } from './types'

/**
 * Install the mock backend with `opts`, run `fn` against the returned
 * handle, and dispose the handle in a `finally` so an assertion or
 * throw in the test body never leaks route handlers or the fake
 * EventSource onto the next spec.
 */
export async function withMockBackend(
  page: Page,
  opts: MockBackendOptions,
  fn: (handle: MockBackendHandle) => Promise<void>,
): Promise<void> {
  const handle = await installMockBackend(page, opts)
  try {
    await fn(handle)
  } finally {
    await handle.dispose()
  }
}

/**
 * Convenience overload for the overwhelming majority of specs that
 * only care about beats. Collapses `{ beats }` into the common arg so
 * test bodies don't repeat it.
 */
export async function withBeats(
  page: Page,
  beats: Beat[],
  fn: (handle: MockBackendHandle) => Promise<void>,
): Promise<void> {
  return withMockBackend(page, { beats }, fn)
}
