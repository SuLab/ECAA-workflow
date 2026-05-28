import { test } from '@playwright/test'
import { runScenario } from '../../helpers/scenarioRunner'

test.describe('Scenario: CRC stool metagenomics cross-cohort meta-analysis', () => {
  test('SME chunked-prose intake → LOCO CV → confirm', async ({ page }) => {
    await runScenario(page, 'fixtures/scenarios/05-metagenomics-crc.yaml')
  })
})
