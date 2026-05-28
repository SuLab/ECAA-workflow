import { test } from '@playwright/test'
import { runScenario } from '../../helpers/scenarioRunner'

test.describe('Scenario: SG-NEx long-read RNA-seq isoform benchmark', () => {
  test('SME chunked-prose intake → per-protocol no-pooling → confirm', async ({
    page,
  }) => {
    await runScenario(page, 'fixtures/scenarios/09-longread-rna-sgnex.yaml')
  })
})
