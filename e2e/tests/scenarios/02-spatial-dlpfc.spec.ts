import { test } from '@playwright/test'
import { runScenario } from '../../helpers/scenarioRunner'

test.describe('Scenario: DLPFC Visium spatial transcriptomics reconstruction', () => {
  test('SME chunked-prose intake → spatialLIBD → DUA → confirm', async ({ page }) => {
    await runScenario(page, 'fixtures/scenarios/02-spatial-dlpfc.yaml')
  })
})
