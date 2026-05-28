import { test } from '@playwright/test'
import { runScenario } from '../../helpers/scenarioRunner'

test.describe('Scenario: Replogle 2022 genome-scale Perturb-seq reanalysis', () => {
  test('SME chunked-prose intake → scPerturb access path → confirm', async ({
    page,
  }) => {
    await runScenario(page, 'fixtures/scenarios/07-perturb-seq-k562.yaml')
  })
})
