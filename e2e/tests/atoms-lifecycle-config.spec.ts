import { expect, test } from '@playwright/test'
import { DEFAULT_AUTO_APPROVE_DISCOVERIES } from '../helpers/atomsLifecycle'

test.describe('atoms lifecycle defaults', () => {
  test('auto-approves common enrichment discovery gates in L1 execution runs', () => {
    expect(DEFAULT_AUTO_APPROVE_DISCOVERIES).toContain('pathway_enrichment')
  })
})
