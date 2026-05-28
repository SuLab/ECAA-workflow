import { test } from '@playwright/test'
import { runScenario } from '../../helpers/scenarioRunner'

test.describe('Scenario: Schizophrenia GWAS coloc with GTEx cis-eQTL', () => {
  test('SME chunked-prose intake → public-tier access guard → confirm', async ({
    page,
  }) => {
    await runScenario(page, 'fixtures/scenarios/04-gwas-scz-coloc.yaml')
  })
})
