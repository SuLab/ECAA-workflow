import { test } from '@playwright/test'
import { runScenario } from '../../helpers/scenarioRunner'

test.describe('Scenario: CPTAC breast cancer proteogenomic reanalysis', () => {
  test('SME chunked-prose intake → TMT-11 IRS normalization → confirm', async ({
    page,
  }) => {
    await runScenario(page, 'fixtures/scenarios/06-proteomics-cptac-brca.yaml')
  })
})
