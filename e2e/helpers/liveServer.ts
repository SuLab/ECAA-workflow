/**
 * Helpers for tier 2 (live) Playwright tests.
 *
 * The live tier boots the real scripps-workflow-server via
 * playwright.config.ts `webServer` (only when `PLAYWRIGHT_LIVE=1`).
 * Tests drive the real UI in Chromium against the real Anthropic API.
 *
 * This module is NOT used by the mocked tier. Mocked tests use
 * helpers/mockBackend.ts instead.
 */

import { mkdtempSync, existsSync, readFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import type { Page, Response } from '@playwright/test'

const __dirname = dirname(fileURLToPath(import.meta.url))
const REPO_ROOT = resolve(__dirname, '..', '..')
const IVD_INTAKE_PATH = join(
  REPO_ROOT,
  'testdata',
  'IVD_prompt',
  'ivd-chat-intake.md',
)

// `SWFC_TEST_PORT` lets parallel CI runners or local devs run multiple live
// servers without colliding on 3737. The server picks the same env var when
// it boots, so the two surfaces stay in sync. (S5.14)
const TEST_PORT = process.env.SWFC_TEST_PORT
  ? Number.parseInt(process.env.SWFC_TEST_PORT, 10)
  : 3737
const BASE_URL = process.env.SWFC_PLAYWRIGHT_BASE_URL ?? `http://127.0.0.1:${TEST_PORT}`

/**
 * Create a fresh temp directory for the emitted package and return its
 * absolute path. Isolated per-test so parallel runs don't collide.
 *
 * NOTE: since the `emit_package` tool's schema intentionally hides
 * `output_dir`, the server picks the path — not the test. Prefer
 * `waitForEmittedPackagePath` for the authoritative post-emit path.
 * mkPackageDir is retained for legacy callers and for harness tests
 * that drive the compiler directly (not through the chat session).
 */
export function mkPackageDir(): string {
  return mkdtempSync(join(tmpdir(), 'scripps-e2e-live-pkg-'))
}

/**
 * Capture the session_id from the /api/chat/session POST response.
 * Must be called BEFORE page.goto('/') via Promise.all so the listener
 * is attached before the request lands.
 */
export async function waitForSessionCreated(page: Page): Promise<string> {
  const response: Response = await page.waitForResponse(
    (resp) =>
      /\/api\/(?:v1\/)?chat\/session$/.test(new URL(resp.url()).pathname) &&
      resp.request().method() === 'POST' &&
      resp.status() === 200,
  )
  const body = (await response.json()) as { session_id: string }
  return body.session_id
}

/**
 * Poll for a package artifact on disk. Returns true when the file exists,
 * false when the timeout is exceeded. Used after clicking Confirm to
 * verify the server actually wrote the package.
 */
export async function waitForPackageArtifact(
  pkgDir: string,
  filename: string,
  timeoutMs = 60_000,
): Promise<boolean> {
  const deadline = Date.now() + timeoutMs
  const target = join(pkgDir, filename)
  while (Date.now() < deadline) {
    if (existsSync(target)) return true
    await new Promise((r) => setTimeout(r, 500))
  }
  return false
}

/**
 * Poll `/api/chat/session/:id/state` until the session reaches the
 * `emitted` state and a path is assigned, then return
 * `emitted_package_path` from the snapshot. Returns `null` on timeout.
 *
 * This is the authoritative way to learn where the package is in
 * live tests: the `emit_package` tool's schema deliberately hides
 * `output_dir`, so tests must not pre-allocate a directory and assert
 * against it. The server assigns the path; we read it back from state.
 */
export async function waitForEmittedPackagePath(
  page: Page,
  sessionId: string,
  timeoutMs = 120_000,
): Promise<string | null> {
  const deadline = Date.now() + timeoutMs
  const url = `${BASE_URL}/api/chat/session/${sessionId}/state`
  while (Date.now() < deadline) {
    const snapshot = await page.evaluate(async (u) => {
      const res = await fetch(u)
      if (!res.ok) return null
      return (await res.json()) as {
        state: { kind: string }
        emitted_package_path?: string
      }
    }, url)
    if (
      snapshot &&
      snapshot.state.kind === 'emitted' &&
      typeof snapshot.emitted_package_path === 'string' &&
      snapshot.emitted_package_path.length > 0
    ) {
      return snapshot.emitted_package_path
    }
    await new Promise((r) => setTimeout(r, 500))
  }
  return null
}

/**
 * IVD prose — single paragraph tuned for the real model. Loaded from
 * testdata/IVD_prompt/ivd-chat-intake.md so every IVD live surface
 * (this helper, the two specs, the scenarios YAML, the shell test)
 * shares one source of truth. HTML comments at the top of the markdown
 * file are stripped before the prose is returned.
 *
 * No `{pkgDir}` substitution: the emit_package tool hides `output_dir`
 * by design. Tests read the server-assigned path from
 * `SessionStateSnapshot.emitted_package_path` via `waitForEmittedPackagePath`.
 */
export function ivdIntakeProse(): string {
  const raw = readFileSync(IVD_INTAKE_PATH, 'utf8')
  return raw.replace(/<!--[\s\S]*?-->\s*/g, '').trim()
}
