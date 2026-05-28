import { defineConfig, devices } from '@playwright/test'

/**
 * Playwright configuration for the scripps-workflow natural-chat UI.
 *
 * Two tiers:
 *  - Default (mocked): vite preview serves the built UI; every test mocks
 *  /api/chat/** via page.route() and a fake EventSource. Fast, deterministic,
 *  no API key required.
 *  - Live (PLAYWRIGHT_LIVE=1): boots the real scripps-workflow-server and
 *  drives the IVD walkthrough against the live Anthropic API. Only the
 *  tests/live/** directory runs in this mode.
 */

const LIVE = process.env.PLAYWRIGHT_LIVE === '1'
// Live-tier server port matches the harness-callback default (3737) so
// E2E tests use the same address the production harness would.
// See docs/api-reference.md "Port Conventions".
const PORT = LIVE ? 3737 : 4173
const BASE_URL = `http://127.0.0.1:${PORT}`
const HARNESS_BIN_PATH =
  process.env.SWFC_HARNESS_BIN_PATH ??
  (process.env.CARGO_TARGET_DIR
    ? `${process.env.CARGO_TARGET_DIR}/debug/scripps-workflow-harness`
    : `${process.cwd()}/../target/debug/scripps-workflow-harness`)

export default defineConfig({
  testDir: LIVE ? './tests/live' : './tests',
  testIgnore: LIVE ? [] : ['**/live/**'],
  fullyParallel: !LIVE,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  // Mocked tier: cap workers at the same CI-proven count locally.
  // Headed Firefox becomes timing-sensitive when Playwright fans out to
  // every available core; the cap keeps headed visual runs deterministic.
  workers: LIVE ? 1 : 4,
  reporter: process.env.CI ? [['github'], ['html', { open: 'never' }]] : 'list',
  // LIVE runs a single long-running full-DAG spec. Disable the per-test
  // timeout cap entirely — the spec itself respects its internal
  // EXECUTION_MAX_WAIT + MAX_BLOCKER_CYCLES budgets. Mocked tier keeps
  // the 30 s cap. Prior regression: any non-zero timeout here will
  // kill a full-DAG run partway through.
  timeout: LIVE ? 0 : 30_000,
  // Route Playwright's auto-generated per-spec artifacts (test-failed
  // screenshots, video, trace, error-context) into our structured
  // test-results tree instead of the default
  // `test-results/ivd-full-lifecycle-headed.-f28a3-…-chromium/` bin,
  // which collided with the `test-results/ivd-full-headed/` dir our
  // spec's `logLine` writes to.
  outputDir: LIVE ? 'test-results/ivd-full-headed/_playwright' : 'test-results',
  expect: {
    // Mocked-tier headroom: lazy state-inspector tabs (commit c378fcc)
    // fetch their chunk on first activation; in cold CI the network +
    // parse can outrun the prior 5s budget. 10s comfortably covers
    // chunk-fetch plus the 4s metrics poll without slowing the green
    // path.
    timeout: LIVE ? 20_000 : 10_000,
  },
  use: {
    baseURL: BASE_URL,
    trace: LIVE ? 'on' : 'on-first-retry',
    screenshot: LIVE ? 'on' : 'only-on-failure',
    video: LIVE ? 'on' : 'retain-on-failure',
    actionTimeout: LIVE ? 15_000 : 5_000,
    navigationTimeout: LIVE ? 30_000 : 10_000,
  },
  projects: LIVE
    ? [
        {
          name: 'chromium',
          use: { ...devices['Desktop Chrome'] },
        },
      ]
    : [
        {
          name: 'chromium',
          use: { ...devices['Desktop Chrome'] },
        },
        {
          name: 'firefox',
          use: { ...devices['Desktop Firefox'] },
        },
        {
          name: 'mobile-chrome',
          use: { ...devices['Pixel 5'] },
          // R-53 — broaden mobile-chrome coverage beyond responsive-layout +
          // the IVD scenario. The confirmation-gate / tool-call feedback /
          // blocker-recovery flows are the load-bearing UX surfaces the SME
          // touches on mobile; the prior testMatch left them desktop-only.
          testMatch:
            /04-responsive-layout\.spec\.ts|02-confirmation-gate\.spec\.ts|05-tool-call-feedback\.spec\.ts|07-blocker-recovery\.spec\.ts|scenarios\/ivd-scrnaseq\.spec\.ts/,
        },
      ],
  webServer: LIVE
    ? {
        // Use `port` instead of `url` so Playwright's readiness check is a
        // plain TCP accept rather than an HTTP probe. The chat routes are
        // POST-only and a GET on /api/chat/session returns 405, which
        // Playwright does not treat as "ready".
        command:
          'cd .. && SWFC_CHAT_SESSIONS_DIR=/tmp/scripps-e2e-sessions cargo run -q -p scripps-workflow-server -- --port 3737',
        port: PORT,
        // Reuse a pre-started server when one is already listening on
        //:3737. Lets long-running full-DAG specs outlive Playwright —
        // operator starts the server standalone, runs the spec, spec
        // exits, server keeps handling harness progress events. When
        // no server is running, Playwright spawns its own (original
        // behavior). Closes the gap where run_complete → webServer
        // SIGTERM → harness loses its backend.
        reuseExistingServer: true,
        timeout: 180_000,
        env: {
          SWFC_CHAT_SESSIONS_DIR: '/tmp/scripps-e2e-sessions',
          // Absolute path so /start-execution finds the harness without
          // relying on PATH. The harness must be pre-built
          // (cargo build -p scripps-workflow-harness) before running the
          // start-execution live spec.
          SWFC_HARNESS_BIN_PATH: HARNESS_BIN_PATH,
          // Individual specs override per-run via SWFC_DEFAULT_AGENT_PATH
          // in process.env before starting Playwright.
          SWFC_DEFAULT_AGENT_PATH:
            process.env.SWFC_DEFAULT_AGENT_PATH ?? 'scripts/agent-claude.sh',
          // Matches DEFAULT_MAX_ITERATIONS in crates/server/src/chat_routes/execution/start.rs.
          SWFC_DEFAULT_MAX_ITERATIONS:
            process.env.SWFC_DEFAULT_MAX_ITERATIONS ?? '20',
          // Keep live harness runs from spending an unbounded number of
          // Claude turns per atom. The agent prompt and post-run wrapper
          // both read this value.
          MAX_TURNS_PER_TASK:
            process.env.MAX_TURNS_PER_TASK ?? '25',
          // Override any .env SWFC_SERVER_URL (which may point at the CLI
          // dev port 3000) so the harness callback URL matches the live
          // test server port. The server reads this on startup and passes
          // it as --server-url to every harness launch.
          SWFC_SERVER_URL: `http://127.0.0.1:${PORT}`,
        },
      }
    : {
        // --host 127.0.0.1 forces IPv4 binding. Default host is
        // `localhost`, which on GitHub-hosted runners resolves IPv6-first
        // (::1), so vite preview ends up listening on::1:4173 only.
        // Playwright's BASE_URL (http://127.0.0.1:4173) hits the v4
        // socket and ERR_CONNECTION_REFUSED loops until the webServer
        // budget elapses. Forcing v4 on both ends fixes it.
        command: 'cd ../ui && npx vite preview --port 4173 --strictPort --host 127.0.0.1',
        // HTTP probe, not TCP accept. vite preview opens the listening
        // socket before its HTTP handler is fully wired; a `port:`-based
        // readiness check signals ready too early and every subsequent
        // page.goto fails with ERR_CONNECTION_REFUSED. `url:` blocks
        // until GET / returns a non-5xx response, which is what we
        // actually need.
        url: BASE_URL,
        reuseExistingServer: !process.env.CI,
        // 3-minute window for cold CI; the 60s budget was too tight
        // once vite preview had to traverse the larger lazy-chunk graph.
        timeout: 180_000,
      },
})
