/**
 * Loads a YAML scenario file, installs the mock backend, navigates the
 * page, and walks every beat.
 *
 * A scenario is JUST DATA. Adding a new real-world scenario is a YAML file
 * plus a one-line spec that calls this runner.
 */

import { readFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import yaml from 'js-yaml'
import type { Page } from '@playwright/test'
import { Chat } from './chat'
import { installMockBackend, type MockBackendHandle } from './mockBackend'
import type { Beat, Scenario } from './types'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')

export function loadScenario(relPath: string): Scenario {
  const abs = resolve(ROOT, relPath)
  const raw = readFileSync(abs, 'utf8')
  const parsed = yaml.load(raw) as Scenario
  validateScenario(parsed, relPath)
  return parsed
}

function validateScenario(s: unknown, path: string): asserts s is Scenario {
  if (!s || typeof s !== 'object') {
    throw new Error(`${path}: scenario must be an object`)
  }
  const scen = s as Partial<Scenario>
  if (!scen.name) throw new Error(`${path}: missing name`)
  if (!scen.modality) throw new Error(`${path}: missing modality`)
  if (!Array.isArray(scen.beats) || scen.beats.length === 0) {
    throw new Error(`${path}: beats must be a non-empty array`)
  }
  for (let i = 0; i < scen.beats.length; i += 1) {
    const b = scen.beats[i] as Partial<Beat>
    if (!b.user) throw new Error(`${path}: beat ${i} missing user`)
    if (!b.assistant || !b.assistant.content) {
      throw new Error(`${path}: beat ${i} missing assistant.content`)
    }
  }
}

export interface RunScenarioOptions {
  /** Override the initial page.goto URL. Default is `/`. */
  url?: string
}

export async function runScenario(
  page: Page,
  scenarioPath: string,
  opts: RunScenarioOptions = {},
): Promise<{ handle: MockBackendHandle; chat: Chat }> {
  const scenario = loadScenario(scenarioPath)

  const handle = await installMockBackend(page, {
    beats: scenario.beats,
    afterConfirmBeats: scenario.finale?.afterConfirm,
  })

  await page.goto(opts.url ?? '/')
  const chat = new Chat(page)

  // Wait for the greeting to render so we know the mock backend is live.
  await chat.waitForAssistant({ timeout: 10_000 })

  for (let i = 0; i < scenario.beats.length; i += 1) {
    const beat = scenario.beats[i]
    await driveBeat(chat, beat, `beat ${i + 1}/${scenario.beats.length}`)
  }

  if (scenario.finale?.confirmAction === 'confirm') {
    await chat.clickConfirm()
    if (scenario.finale.afterConfirm) {
      for (let i = 0; i < scenario.finale.afterConfirm.length; i += 1) {
        const beat = scenario.finale.afterConfirm[i]
        // The post-confirm follow-up turn is driven automatically by
        // useConversation.confirm(), which fires a "(confirmed — please
        // continue)" turn. We wait for its assistant response instead of
        // sending a new user message.
        if (i === 0) {
          await chat.waitForAssistant({ textContains: beat.assistant.content.slice(0, 20) })
          await runBeatExpectations(chat, beat, `afterConfirm ${i + 1}`)
        } else {
          await driveBeat(chat, beat, `afterConfirm ${i + 1}`)
        }
      }
    }
  } else if (scenario.finale?.confirmAction === 'reject') {
    await chat.clickReject()
  }

  return { handle, chat }
}

async function driveBeat(chat: Chat, beat: Beat, label: string): Promise<void> {
  await chat.sendUserMessage(beat.user)
  await chat.waitForAssistant({
    textContains: beat.assistant.content.slice(0, 24),
  })
  await runBeatExpectations(chat, beat, label)
}

async function runBeatExpectations(
  chat: Chat,
  beat: Beat,
  label: string,
): Promise<void> {
  const exp = beat.expect
  if (!exp) return
  try {
    if (exp.visibleText) {
      for (const t of exp.visibleText) await chat.expect.messageContains(t)
    }
    if (exp.forbiddenText) {
      for (const t of exp.forbiddenText)
        await chat.expect.noAssistantMessageContains(t)
    }
    if (exp.stateBadge) await chat.expect.stateBadge(exp.stateBadge)
    if (exp.confirmationCardVisible === true)
      await chat.expect.confirmationCardVisible()
    if (exp.confirmationCardVisible === false)
      await chat.expect.confirmationCardHidden()
    if (exp.blockerVisible === true) await chat.expect.blockerVisible()
    if (exp.blockerVisible === false) await chat.expect.blockerHidden()
    if (exp.infraBannerVisible === true) await chat.expect.infraBannerVisible()
    if (exp.activeTab) await chat.expect.activeTab(exp.activeTab)
    if (exp.jobsBadgeCount !== undefined)
      await chat.expect.jobsBadgeCount(exp.jobsBadgeCount)
    if (exp.pillStatusLine !== undefined)
      await chat.expect.toolPillStatus(exp.pillStatusLine)
  } catch (err) {
    throw new Error(`[${label}] ${(err as Error).message}`)
  }
}
