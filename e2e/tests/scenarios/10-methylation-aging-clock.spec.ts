import { test } from '@playwright/test'
import { runScenario } from '../../helpers/scenarioRunner'

test.describe('Scenario: 450k methylation epigenetic aging clock', () => {
  test('SME chunked-prose intake → Horvath + Hannum → confirm', async ({ page }) => {
    await runScenario(page, 'fixtures/scenarios/10-methylation-aging-clock.yaml')
  })
})
