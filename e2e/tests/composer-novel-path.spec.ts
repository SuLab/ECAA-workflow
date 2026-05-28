// Mock-backend Playwright spec for the novel-composition
// (backward-chain) composer path. When ECAA_COMPOSER=backward-chain
// fires, no archetype matches the goal and the composer plans bottom-
// up from the goal's (edam_data, edam_format) tuple. The MockLlmBackend
// fixture supplies a deterministic chat transcript that exercises the
// path; the assertion is that the resulting WORKFLOW.json carries
// `matched_archetype: null` (the legacy path emits a non-null value).
//
// Pairs with S7.18 — same scenario gated on ECAA_LIVE_API=1 against the
// real Anthropic backend — under e2e/tests/live/.
//
// This spec is a **placeholder** that documents the shape without
// driving the backend — the mock fixture corpus does not yet carry
// a backward-chain scenario. Once the golden corpus is in place,
// the test body activates against the scenarioRunner the same way
// the existing scenario specs do.

import { test } from '@playwright/test'

test.describe('Composer: novel backward-chain path (mock)', () => {
  test.skip(
    true,
    'Pending S6.20 Phase 2 golden corpus + a backward-chain scenario fixture — \
spec scaffolds the placeholder so CI test-count tracking sees it.'
  )

  test('ECAA_COMPOSER=backward-chain produces an archetype-less composition', async ({
    page,
  }) => {
    void page
    // Pending the fixture wiring described above. The activation diff
    // when S6.20 lands will be:
    // await runScenario(page, 'fixtures/scenarios/composer-novel-path.yaml')
    // plus a goldens-comparison check against the WORKFLOW.json snapshot
    // committed under tests/golden-workflows/edge-cases/composer-novel-path/.
  })
})
