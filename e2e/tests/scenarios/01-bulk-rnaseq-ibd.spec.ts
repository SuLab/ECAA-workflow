import { test } from '@playwright/test'
import { runScenario } from '../../helpers/scenarioRunner'

test.describe('Scenario: Public IBD bulk transcriptomics meta-analysis', () => {
  test('SME chunked-prose intake → drug-class stratification → confirm', async ({
    page,
  }) => {
    await runScenario(page, 'fixtures/scenarios/01-bulk-rnaseq-ibd.yaml')
  })
})
