# scripps-workflow-e2e — Playwright end-to-end tests

End-to-end tests for the natural-chat web UI under `ui/`. Two tiers:

- **Mocked (default)** — vite preview serves the built UI. Every test mocks
  `/api/chat/**` via `page.route()` and a fake EventSource installed before
  navigation. No server boot, no API key, no network. UI behavior specs
  (`tests/01-NN-*.spec.ts` — conversational flow, confirmation gate, state
  inspector, responsive layout, tool-call feedback, error recovery, blocker
  recovery, quick replies, harness progress, metrics tab, a11y keyboard
  navigation, settings routing, branch popup) plus scenario-driven specs
  (`tests/scenarios/` — IVD scRNA-seq + the 12 public scenarios under
  `testdata/scenarios/`) and standalone composer / cross-version-diff /
  pilot / stall coverage. Runs on every PR. Authoritative spec + case
  count in [`.github/ci/expected-test-counts.json::playwright_mocked_total`](../.github/ci/expected-test-counts.json) (currently 151 cases).
- **Live (`PLAYWRIGHT_LIVE=1`)** — boots the real `scripps-workflow-server`
  via Playwright's `webServer` config and drives the IVD walkthrough plus
  every scenario against the live Anthropic API. Authoritative count in
  [`expected-test-counts.json::playwright_live_total`](../.github/ci/expected-test-counts.json) (currently 28 cases). Runs on demand and nightly.

## Quickstart

```bash
# From the repo root
make e2e-playwright-install        # installs @playwright/test + browsers once
make e2e-playwright                # tier 1 — full mocked suite
make e2e-playwright-live           # tier 2 — full live suite (needs SWFC_ANTHROPIC_API_KEY)
```

From inside `e2e/`:

```bash
npm install
npm run install-browsers           # one-time Chromium + Firefox download
npm test                           # all mocked projects
npm run test:chromium              # chromium only
npm run test:mobile                # Pixel 5 viewport — responsive suite only
npm run test:headed                # with visible browser windows
npm run test:ui                    # Playwright UI mode (watch + time-travel)
npm run test:live                  # live tier (requires SWFC_ANTHROPIC_API_KEY)
```

## Layout

```
e2e/
  playwright.config.ts             — projects, webServer, timeouts
  helpers/
    selectors.ts                   — single source of truth for ARIA selectors
    types.ts                       — Beat, Scenario, MockTurn, SessionStateKind
    sseStream.ts                   — fake EventSource installed via addInitScript
    mockBackend.ts                 — installs page.route() handlers for /api/chat/**
    chat.ts                        — sendUserMessage, waitForAssistant, expect helpers
    scenarioRunner.ts              — loads YAML scenarios, walks beats, asserts
  fixtures/
    scenarios/                     — YAML beat files, one per real-world scenario
    snippets/                      — small canned responses for unit-style specs
  tests/
    01-conversational-flow.spec.ts
    02-confirmation-gate.spec.ts
    ...
    scenarios/                     — one spec per real-world scenario
    live/                          — live tier (gated on PLAYWRIGHT_LIVE=1)
```

## Adding a new scenario

1. Create a YAML file under `fixtures/scenarios/`. See `ivd-scrnaseq.yaml`
   for the beat schema.
2. Create a one-line spec under `tests/scenarios/` that imports
   `runScenario` and points it at the YAML.

```ts
// tests/scenarios/my-new-scenario.spec.ts
import { test } from '@playwright/test'
import { runScenario } from '../../helpers/scenarioRunner'

test('My scenario description', async ({ page }) => {
  await runScenario(page, 'fixtures/scenarios/my-new-scenario.yaml')
})
```

The runner handles mock backend installation, the navigation, and every
beat assertion. Authoring a new scenario is therefore a YAML edit — no
new TypeScript required.

## Selectors

Every selector used by the suite lives in `helpers/selectors.ts` and
points at existing ARIA labels in the UI components. We do **not** add
`data-testid` attributes for tests — the accessibility audit already
ensured every surface is addressable via semantic roles and labels.

## Troubleshooting

- **Mock backend not hit** — the mock intercepts the `baseURL` from
  `playwright.config.ts`. Make sure `page.goto('/')` not an absolute URL.
- **EventSource not mocked** — `installMockBackend` must run before the
  first `page.goto()`. See `mockBackend.ts` for the init script.
- **Live tier timeout** — first run compiles the Rust server; expect
  30–60 s before the first request lands. The `webServer.timeout` is
  set to 120 s.
- **Browsers missing** — run `npm run install-browsers`. The `make
  e2e-playwright-install` target also does this.

## Environment variables

- `SWFC_ANTHROPIC_API_KEY` — canonical key for the live tier. The legacy
  `ANTHROPIC_API_KEY` name still works as a fallback (one-time stderr
  deprecation warning on first read), but new scripts and CI lanes
  should prefer the prefixed name so the chat-side key doesn't collide
  with Claude Code CLI's own `ANTHROPIC_API_KEY` scan.
- `PLAYWRIGHT_LIVE=1` — gate for the live suite (also implied by the
  Make targets listed above).

## See also

- `docs/accessibility-audit.md` — the a11y baseline
- `scripts/test-ivd-web-execute.sh` — the curl equivalent of the live tier
