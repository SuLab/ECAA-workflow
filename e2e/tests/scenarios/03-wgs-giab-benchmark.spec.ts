import { test } from '@playwright/test'
import { runScenario } from '../../helpers/scenarioRunner'

test.describe('Scenario: GIAB HG002 germline variant calling benchmark', () => {
  test('SME chunked-prose intake → GATK + DeepVariant → confirm', async ({ page }) => {
    await runScenario(page, 'fixtures/scenarios/03-wgs-giab-benchmark.yaml')
  })
})
