import { readFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { expect, test } from '@playwright/test'
import yaml from 'js-yaml'
import { Chat } from '../../helpers/chat'
import { installMockBackend } from '../../helpers/mockBackend'
import type { Scenario } from '../../helpers/types'

/**
 * Scenario 08 — MIMIC-IV sepsis is a special case: the first beat
 * transitions the session to `Blocked` because MIMIC-IV is
 * credentialed-access on PhysioNet. The SME must click the unblock
 * button in BlockerCard before the intake can continue.
 *
 * This spec drives the full flow manually (without scenarioRunner)
 * because the runner has no built-in concept of blocker recovery —
 * letting the runner stay simple and the special case stay visible.
 */

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..')

function loadYamlScenario(relPath: string): Scenario {
  const abs = resolve(ROOT, relPath)
  return yaml.load(readFileSync(abs, 'utf8')) as Scenario
}

test.describe('Scenario: MIMIC-IV sepsis early-warning prediction (with blocker recovery)', () => {
  test('blocked on credentialed access → unblock → continue → confirm', async ({
    page,
  }) => {
    const scenario = loadYamlScenario('fixtures/scenarios/08-ehr-sepsis-mimic.yaml')
    const [blockerBeat, ...remainingBeats] = scenario.beats

    const handle = await installMockBackend(page, {
      beats: scenario.beats,
      afterConfirmBeats: scenario.finale?.afterConfirm,
      unblockTarget: { kind: 'intake_followup' },
    })
    try {
      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      // Beat 1 — the intake that hits the credentialed-access gate.
      await chat.sendUserMessage(blockerBeat.user)
      await chat.waitForAssistant({
        textContains: blockerBeat.assistant.content.slice(0, 24),
      })
      await chat.expect.stateBadge('blocked')
      await chat.expect.blockerVisible()
      await chat.expect.messageContains('credentialed')
      await chat.expect.messageContains('DUA')

      // SME confirms DUA out-of-band and clicks the unblock button.
      // This advances the mocked state to intake_followup.
      await chat.clickUnblock()
      await chat.expect.stateBadge('intake_followup')
      await chat.expect.blockerHidden()

      // Beats 2..N — continue through the intake after unblock.
      for (let i = 0; i < remainingBeats.length; i += 1) {
        const beat = remainingBeats[i]
        await chat.sendUserMessage(beat.user)
        await chat.waitForAssistant({
          textContains: beat.assistant.content.slice(0, 24),
        })
        if (beat.expect?.visibleText) {
          for (const t of beat.expect.visibleText) {
            await chat.expect.messageContains(t)
          }
        }
        if (beat.expect?.forbiddenText) {
          for (const t of beat.expect.forbiddenText) {
            await chat.expect.noAssistantMessageContains(t)
          }
        }
        if (beat.expect?.stateBadge)
          await chat.expect.stateBadge(beat.expect.stateBadge)
        if (beat.expect?.confirmationCardVisible)
          await chat.expect.confirmationCardVisible()
      }

      // Finale — click Confirm, then wait for the post-confirm turn.
      expect(scenario.finale?.confirmAction).toBe('confirm')
      await chat.clickConfirm()
      const afterConfirmBeat = scenario.finale!.afterConfirm![0]
      await chat.waitForAssistant({
        textContains: afterConfirmBeat.assistant.content.slice(0, 24),
      })
      await chat.expect.stateBadge('emitted')
    } finally {
      await handle.dispose()
    }
  })
})
