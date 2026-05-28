import { test } from '@playwright/test'
import { runScenario } from '../../helpers/scenarioRunner'

test.describe('Scenario: Mock Phase III clinical-trial analysis', () => {
  test('SME SAP prose → clinical-trial taxonomy loads → confirm', async ({ page }) => {
    await runScenario(page, 'fixtures/scenarios/11-clinical-trial-mock-phase3.yaml')
  })
})
