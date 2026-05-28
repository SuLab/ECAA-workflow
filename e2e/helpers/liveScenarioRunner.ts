/**
 * Live scenario runner — drives real multi-turn conversations through
 * the actual scripps-workflow-server against the Anthropic API.
 *
 * The runner walks the scenario's `beats` array one at a time: send
 * beat[0].user, wait for the model to respond, send beat[1].user, wait,
 * and so on. This produces a natural question-and-answer flow where the
 * SME provides information incrementally and the system asks clarifying
 * questions between rounds — exactly how a real domain expert would
 * interact with the intake surface.
 *
 * After all beats are sent, the runner drives the deterministic
 * confirmation + emit flow and verifies package artifacts on disk.
 */

import { existsSync, mkdirSync } from 'node:fs'
import { join } from 'node:path'
import { expect, type Page } from '@playwright/test'
import { Chat } from './chat'
import { chooseLiveFollowup } from './liveFollowups'
import { waitForEmittedPackagePath, waitForSessionCreated } from './liveServer'
import { loadScenario } from './scenarioRunner'
import { sel } from './selectors'

const BASE_URL = process.env.SWFC_PLAYWRIGHT_BASE_URL ?? 'http://127.0.0.1:3737'
const TURN_TIMEOUT = 240_000
const PACKAGE_TIMEOUT = 120_000
const CONFIRMATION_FALLBACK_REPLY =
  'That is correct. Please prepare the confirmation card so I can review and approve the package.'

export interface LiveScenarioResult {
  /** UUID of the created chat session. Useful for post-emit lifecycle steps. */
  sessionId: string
}

export interface LiveScenarioOptions {
  /**
   * Optional setup hook after the page has created a session and rendered the
   * greeting, but before the first SME intake turn is sent.
   */
  beforeFirstTurn?: (ctx: {
    page: Page
    sessionId: string
    screenshotDir: string
  }) => Promise<void>
}

export async function runLiveScenario(
  page: Page,
  scenarioPath: string,
  options: LiveScenarioOptions = {},
): Promise<LiveScenarioResult> {
  const scenario = loadScenario(scenarioPath)
  const live = scenario.live
  if (!live) {
    throw new Error(`${scenarioPath}: no 'live:' section — cannot run as a live test`)
  }

  const screenshotDir = join('test-results', 'screenshots', scenario.name.replace(/[^a-zA-Z0-9_-]/g, '_'))
  mkdirSync(screenshotDir, { recursive: true })

  // ── 1. Navigate and capture session ─────────────────────────────────
  const sessionIdPromise = waitForSessionCreated(page)
  await page.goto(`${BASE_URL}/`)
  const sessionId = await sessionIdPromise
  expect(sessionId).toMatch(/^[0-9a-f-]{36}$/)

  const chat = new Chat(page)
  await chat.waitForAssistant({ timeout: 30_000 })
  if (options.beforeFirstTurn) {
    await options.beforeFirstTurn({ page, sessionId, screenshotDir })
  }

  // ── 2. Walk the beats as a natural multi-turn conversation ──────────
  // Each beat is one SME message followed by waiting for the system's
  // response. This produces the natural Q&A flow the intake surface is
  // designed for — short messages, clarifying questions, progressive
  // detail accumulation.
  let latestAssistantText = ''
  for (let i = 0; i < scenario.beats.length; i += 1) {
    const beat = scenario.beats[i]
    const userText = beat.user.trim()

    console.log(`\n── Beat ${i + 1}/${scenario.beats.length} ──`)
    console.log(`  USER: ${userText.slice(0, 80)}${userText.length > 80 ? '…' : ''}`)

    await chat.sendUserMessage(userText)
    await waitForComposerEnabled(page)

    latestAssistantText = await chat.latestAssistant().textContent() ?? ''
    console.log(`  ASSISTANT: ${latestAssistantText.slice(0, 120)}${latestAssistantText.length > 120 ? '…' : ''}`)
    console.log(`  STATE: ${await fetchState(page, sessionId)}`)

    // Screenshot after each beat for video-style analysis.
    await page.screenshot({ path: join(screenshotDir, `beat-${i + 1}.png`), fullPage: true })

    // Per-beat forbidden text checks on the latest assistant response.
    if (beat.expect?.forbiddenText) {
      for (const text of beat.expect.forbiddenText) {
        await chat.expect.noAssistantMessageContains(text)
      }
    }
  }

  // ── 3. State check ──────────────────────────────────────────────────
  const stateAfterIntake = await fetchState(page, sessionId)
  if (live.assertions.stateNotGreeting) {
    expect(
      stateAfterIntake,
      'state should have advanced past greeting after intake',
    ).not.toBe('greeting')
  }

  // ── 4. Global forbidden-text scan ───────────────────────────────────
  if (live.assertions.forbiddenText) {
    for (const text of live.assertions.forbiddenText) {
      await chat.expect.noAssistantMessageContains(text)
    }
  }

  // ── 5. Auto-approve any awaiting-signoff propose_hypothesized_node
  // proposals before confirm. Mirrors the SME clicking "Approve &
  // promote" on every HypothesizedProposalCard. Without this,
  // proposal-heavy scenarios (CODEX, niche modalities, Tier-B flex)
  // stall at pending_confirmation because emit_package refuses to
  // run while any proposal.lifecycle is still AwaitingSignoff.
  // No-op for scenarios that don't surface proposals.
  await page.evaluate(
    async (urls: { list: string; signoff: string }) => {
      const list = await fetch(urls.list).then((r) =>
        r.ok ? r.json() : [],
      )
      const arr = Array.isArray(list) ? list : []
      for (const p of arr) {
        const lc = (p as { lifecycle?: { kind?: string } | string }).lifecycle
        const kind = typeof lc === 'string' ? lc : (lc?.kind ?? '')
        if (kind === 'awaiting_signoff') {
          const pid = (p as { id?: string }).id
          if (!pid) continue
          await fetch(`${urls.signoff}/${pid}/signoff`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ sme_initials: 'e2e' }),
          })
        }
      }
    },
    {
      list: `${BASE_URL}/api/chat/session/${sessionId}/proposals`,
      signoff: `${BASE_URL}/api/chat/session/${sessionId}/proposal`,
    },
  )

  // ── 5b. Adaptive intake follow-ups ──────────────────────────────────
  // Live LLM turns do not always render the confirmation card after the
  // fixed scenario beats. Read the latest assistant text and answer only
  // questions it actually asks, with a small cap to avoid burning tokens.
  if (live.assertions.directConfirm || live.assertions.verifyPackage) {
    const usedTriggers = new Set<string>()
    const maxFollowups = maxAdaptiveFollowups()

    for (let i = 0; i < maxFollowups; i += 1) {
      const state = await fetchState(page, sessionId)
      if (state === 'pending_confirmation' || state === 'ready_to_emit') {
        console.log(`  ADAPTIVE INTAKE: reached ${state}; continuing`)
        break
      }

      const picked = chooseLiveFollowup(
        live.followups,
        latestAssistantText,
        usedTriggers,
        CONFIRMATION_FALLBACK_REPLY,
      )
      if (picked.kind === 'scenario') usedTriggers.add(picked.key)

      console.log(
        `  ADAPTIVE INTAKE ${i + 1}/${maxFollowups}: ${picked.kind}:${picked.key}`,
      )
      console.log(`  SME: ${picked.reply.slice(0, 120)}${picked.reply.length > 120 ? '…' : ''}`)

      await chat.sendUserMessage(picked.reply)
      await waitForComposerEnabled(page)
      latestAssistantText = await chat.latestAssistant().textContent() ?? ''
      console.log(`  ASSISTANT: ${latestAssistantText.slice(0, 120)}${latestAssistantText.length > 120 ? '…' : ''}`)
      console.log(`  STATE: ${await fetchState(page, sessionId)}`)
      await page.screenshot({
        path: join(screenshotDir, `adaptive-followup-${i + 1}.png`),
        fullPage: true,
      })
    }
  }

  if (live.assertions.forbiddenText) {
    for (const text of live.assertions.forbiddenText) {
      await chat.expect.noAssistantMessageContains(text)
    }
  }

  // ── 5b. Confirmation ────────────────────────────────────────────────
  // Second proposal-signoff sweep right before /confirm. The adaptive
  // intake follow-up loop above can surface new propose_hypothesized_node
  // tool calls — the first sweep at line 128 only catches proposals from
  // the initial beats. Without this second pass, emit_package refuses
  // with `emit refused: N proposal(s) still pending SME action` for any
  // scenario whose follow-ups push the LLM to propose novel nodes
  // (cross-omics-rna-atac is the canonical reproducer).
  await page.evaluate(
    async (urls: { list: string; signoff: string }) => {
      const list = await fetch(urls.list).then((r) =>
        r.ok ? r.json() : [],
      )
      const arr = Array.isArray(list) ? list : []
      for (const p of arr) {
        const lc = (p as { lifecycle?: { kind?: string } | string }).lifecycle
        const kind = typeof lc === 'string' ? lc : (lc?.kind ?? '')
        if (kind === 'awaiting_signoff') {
          const pid = (p as { id?: string }).id
          if (!pid) continue
          await fetch(`${urls.signoff}/${pid}/signoff`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ sme_initials: 'e2e' }),
          })
        }
      }
    },
    {
      list: `${BASE_URL}/api/chat/session/${sessionId}/proposals`,
      signoff: `${BASE_URL}/api/chat/session/${sessionId}/proposal`,
    },
  )

  let confirmedViaUi = false
  if (live.assertions.directConfirm) {
    // Default is to click the actual confirmation button so the run mirrors
    // SME behaviour. SWFC_E2E_CONFIRM_VIA_FETCH=1 opts back into the
    // historical raw-POST bypass (fast-feedback dev mode, not SME-realistic).
    const confirmViaFetch = process.env.SWFC_E2E_CONFIRM_VIA_FETCH === '1'
    if (!confirmViaFetch) {
      await chat.clickConfirm()
      confirmedViaUi = true
      await expect(page.locator(sel.confirmButton)).toHaveCount(0, {
        timeout: 10_000,
      })
    } else {
      const confirmStatus = await page.evaluate(
        async (url) => {
          const res = await fetch(url, { method: 'POST' })
          return res.status
        },
        `${BASE_URL}/api/chat/session/${sessionId}/confirm`,
      )
      expect(
        confirmStatus,
        '/confirm should return 204 (session must be in a confirmation-eligible state)',
      ).toBe(204)
    }
  }

  // ── 6. Post-confirm emit ────────────────────────────────────────────
  // The emit_package tool's schema hides `output_dir`; the server picks
  // the path, exposes it through SessionStateSnapshot.emitted_package_path,
  // And we poll for the emitted state to learn where artifacts.
  // No pkgDir pre-allocation and no retry: the read is authoritative.
  //
  // When verifyPackage is false (refusal scenarios for explicit-only
  // taxonomies like gwas-coloc), we still send a follow-up turn so the
  // LLM surfaces its refusal rationale, then assert on the refusal text.
  let pkgDir = ''
  if (!live.assertions.verifyPackage && live.assertions.refusalTextContains) {
    await chat.sendUserMessage('Confirmed — please continue and emit the package.')
    await waitForComposerEnabled(page)

    const finalResponse = (await chat.latestAssistant().textContent()) ?? ''
    console.log(`  REFUSAL RESPONSE: ${finalResponse.slice(0, 120)}…`)

    const needles = live.assertions.refusalTextContains
    const matched = needles.some((n) =>
      finalResponse.toLowerCase().includes(n.toLowerCase()),
    )
    expect(
      matched,
      `refusal scenario: final assistant bubble did not contain any of ${JSON.stringify(
        needles,
      )}`,
    ).toBe(true)
  }
  if (live.assertions.verifyPackage) {
    if (!confirmedViaUi) {
      await chat.sendUserMessage('Confirmed — please continue and emit the package.')
      await waitForComposerEnabled(page)
    }

    // Diagnostic: log what the model said on the post-confirm turn so
    // emit-path failures are diagnosable instead of a silent timeout.
    const postConfirmResponse = await chat.latestAssistant().textContent() ?? ''
    const postConfirmState = await fetchState(page, sessionId)
    console.log(`  POST-CONFIRM RESPONSE: ${postConfirmResponse.slice(0, 120)}…`)
    console.log(`  POST-CONFIRM STATE: ${postConfirmState}`)

    const emittedPath = await waitForEmittedPackagePath(page, sessionId, PACKAGE_TIMEOUT)
    expect(
      emittedPath,
      'session must reach emitted with emitted_package_path set',
    ).not.toBeNull()
    pkgDir = emittedPath!
    console.log(`  EMITTED PATH: ${pkgDir}`)

    for (const f of [
      'WORKFLOW.json',
      'PROMPT.md',
      'CONTEXT.md',
      'ro-crate-metadata.json',
    ]) {
      expect(existsSync(join(pkgDir, f)), `missing artifact: ${f}`).toBe(true)
    }

    const auditLog = join(pkgDir, 'runtime', 'intake-conversation.jsonl')
    expect(existsSync(auditLog), 'intake-conversation.jsonl must exist').toBe(true)

    // Screenshot: capture the chat pane after emission.
    await page.screenshot({ path: join(screenshotDir, 'after-emit-chat.png'), fullPage: true })
  }

  // ── 6b. DAG display verification ───────────────────────────────────
  // Switch to the Plan tab and verify the DAG canvas renders with
  // task nodes. The new /api/chat/session/:id/dag endpoint serves the
  // session's in-memory DAG, so it works during intake (no harness
  // Needed). The xyflow canvas renders.react-flow__node for each task.
  if (live.assertions.verifyPackage) {
    await chat.openTab('plan')
    // Wait for DagCanvas to fetch the DAG and render xyflow nodes.
    await page.waitForFunction(
      () => document.querySelectorAll('.react-flow__node').length > 0,
      { timeout: 10_000 },
    ).catch(() => {
      // If no nodes after 10s, continue — screenshot will show the state.
    })

    const nodeCount = await page.locator('.react-flow__node').count()
    console.log(`  DAG NODES RENDERED: ${nodeCount}`)
    await page.screenshot({ path: join(screenshotDir, 'dag-plan-tab.png'), fullPage: true })
    expect(nodeCount, 'Plan tab should show DAG task nodes').toBeGreaterThan(0)
  }

  // ── 7. Final state assertion ────────────────────────────────────────
  if (live.assertions.expectedFinalState) {
    const finalState = await fetchState(page, sessionId)
    expect(finalState).toBe(live.assertions.expectedFinalState)
  }

  // ── 8. Transcript length ────────────────────────────────────────────
  if (live.assertions.minTranscriptTurns) {
    const turns = await page.evaluate(
      async (url) => {
        const res = await fetch(url)
        const body = (await res.json()) as unknown[]
        return body.length
      },
      `${BASE_URL}/api/chat/session/${sessionId}/transcript`,
    )
    expect(turns).toBeGreaterThanOrEqual(live.assertions.minTranscriptTurns)
  }

  return { sessionId }
}

async function waitForComposerEnabled(page: Page): Promise<void> {
  await expect(page.locator(sel.composer)).toBeEnabled({
    timeout: TURN_TIMEOUT,
  })
}

async function fetchState(page: Page, sessionId: string): Promise<string> {
  return page.evaluate(
    async (url) => {
      const res = await fetch(url)
      const body = (await res.json()) as { state: { kind: string } }
      return body.state.kind
    },
    `${BASE_URL}/api/chat/session/${sessionId}/state`,
  )
}

function maxAdaptiveFollowups(): number {
  const raw = process.env.SWFC_E2E_MAX_INTAKE_FOLLOWUPS ?? '4'
  const parsed = Number.parseInt(raw, 10)
  if (!Number.isFinite(parsed) || parsed < 0) return 4
  return parsed
}
