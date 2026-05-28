import { test } from '@playwright/test'
import { runScenario } from '../../helpers/scenarioRunner'

test.describe('Scenario: Mock SARIMA monthly forecast', () => {
  test('SME time-series prose → forecast taxonomy loads → confirm', async ({ page }) => {
    await runScenario(page, 'fixtures/scenarios/12-time-series-forecast.yaml')
  })
})
