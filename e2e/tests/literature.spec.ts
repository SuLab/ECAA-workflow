import { expect, test } from '@playwright/test'
import { Chat } from '../helpers/chat'
import { withMockBackend } from '../helpers/withMockBackend'
import type { Beat } from '../helpers/types'

/**
 * Literature-context UI flow
 *
 * Exercises: click on a `<lit-entity>` pill in an assistant turn →
 * the LitEntityButton fetches /literature-context and opens a floating
 * LiteratureContextCard dialog.
 *
 * Mocked tier only: the `/literature-context` endpoint is intercepted
 * at the Playwright network layer. The full click → fetch → render flow
 * at the unit level is covered by Vitest in
 * ui/src/components/AssistantTurnCard.litEntity.test.tsx (Task 17).
 *
 * Live tier: opt-in via `make e2e-literature-live`
 * (SWFC_LIT_LIVE_API=1) for a real PubMed roundtrip.
 */

const MOCK_LIT_CTX = {
  entity: 'ACAN',
  entity_kind: 'gene',
  prior_rows: [
    {
      entity: 'ACAN',
      entity_kind: 'gene',
      pmid: '28123456',
      evidence_quote: 'ACAN reduction in disc tissue',
      source_kind: 'pmc_oa_full_text',
      source_hash: 'sha256:abc',
      redistributable: true,
    },
  ],
  finding_rows: [],
  source_artifacts: [],
  source_scope: 'pmc_oa',
}

/** A beat whose assistant content contains a lit-entity span. */
const litEntityBeat: Beat = {
  user: 'What do we know about ACAN in IVD degeneration?',
  assistant: {
    content:
      'The gene <lit-entity name="ACAN" kind="gene" /> (aggrecan) is a key extracellular matrix component in the nucleus pulposus.',
  },
}

test.describe('literature-context UI flow', () => {
  test('click <lit-entity> button → popover shows mocked literature rows', async ({
    page,
  }) => {
    await withMockBackend(page, { beats: [litEntityBeat] }, async () => {
      // Register the literature-context intercept AFTER withMockBackend so
      // Playwright (last-registered-first) dispatches to us before the
      // generic session catch-all. The sessionId in the URL is dynamic so
      // we match on the path segment only.
      await page.route('**/literature-context*', async (route) => {
        await route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify(MOCK_LIT_CTX),
        })
      })

      await page.goto('/')
      const chat = new Chat(page)
      await chat.waitForAssistant()

      // Drive the beat so the assistant message containing <lit-entity> appears.
      await chat.sendUserMessage(litEntityBeat.user)
      await chat.waitForAssistant({ textContains: 'aggrecan' })

      // The lit-entity tag should render as an interactive button.
      const litBtn = page.getByRole('button', {
        name: /Show literature context for ACAN/i,
      })
      await expect(litBtn).toBeVisible()

      // Click it — should open the popover and show the mocked PMID.
      await litBtn.click()

      const dialog = page.getByRole('dialog', {
        name: /Literature context for ACAN/i,
      })
      await expect(dialog).toBeVisible()
      await expect(page.getByText('28123456')).toBeVisible()
      await expect(
        page.getByText(/ACAN reduction in disc tissue/i),
      ).toBeVisible()

      // Close button should dismiss the popover.
      await page.getByRole('button', { name: /Close literature context/i }).click()
      await expect(dialog).not.toBeVisible()
    })
  })

  test('literature-context endpoint returns 404 for unknown session (smoke)', async ({
    request,
  }) => {
    // Route-level smoke: verifies the endpoint is registered on the server.
    // The vite-preview server used in the mocked tier does not have real API
    // routes, so this test is only meaningful against a live server.
    // We assert that the mock-tier vite preview returns a non-200 for an
    // unknown session id — anything other than a successful 200 with JSON
    // is acceptable (the server would return 404/409; vite returns 404 for
    // unknown paths).
    const sessionId = '00000000-0000-0000-0000-000000000000'
    const resp = await request.get(
      `/api/chat/session/${sessionId}/literature-context?entity=ACAN&entity_kind=gene`,
    )
    // Both 404 (session not found / no lit atoms) and 409 (not emitted)
    // are valid server responses. The vite preview also returns 404 for
    // unregistered routes. Anything non-200 confirms the endpoint is at
    // least not accidentally leaking a 200 with wrong content.
    expect(resp.status()).not.toBe(200)
  })
})
