# ECAA-workflow UI

> **This file is for frontend developers.** Domain scientists should start with [`../USERS.md`](../USERS.md) — not this README.

React + Vite + TypeScript frontend for `ecaa-workflow-server`. A natural-chat conversation surface: the SME describes their project in plain prose, the server drives an LLM-mediated tool-use loop against the deterministic compiler, and the UI renders the chat timeline, the planning DAG, and live harness progress in a split-pane layout.

There are **no forms, no modals, and no DAG vocabulary in the user-facing surface**. Confirmation, SME input, structured capture, and blocker handling all render as in-chat turn cards.

## Stack

- **React 18** (function components, hooks only — no class components)
- **Vite 5** (dev server, HMR, production build)
- **TypeScript 5**
- **Vitest** + **@testing-library/react** + **axe-core** for unit and a11y tests
- **`@xyflow/react`** for the DAG canvas (used only in `DagCanvas.tsx`)
- **`@dagrejs/dagre`** for automatic node layout
- **`react-virtuoso`** for virtualized long-list scrolling (`ConversationPane`, decision log, task drawer). Wrap items in a `<Virtuoso />` component when the list can grow past ~50 entries; manual rendering of every node thrashes layout when a heavy session has hundreds of turns.

No router, no global state library, no CSS-in-JS runtime. Inline styles and plain React state only.

## Dev workflow

From the repo root:

```bash
make dev-server    # Rust backend on :3000 (terminal 1)
make dev-ui        # Vite dev server on :5173 (terminal 2)
```

Open <http://localhost:5173>. Vite proxies `/api/*` to `localhost:3000` (see `vite.config.ts`).

From inside `ui/`:

```bash
npm install
npm run dev        # same as `make dev-ui`
npm run build      # tsc && vite build — writes to ui/dist/
```

`make test-ui` (from the repo root) runs the Vitest suite; `make check` runs `tsc --noEmit` on top of `cargo test`. Regenerate ts-rs bindings with `make types` after any change to a `#[derive(TS)]` type in `crates/core` or `crates/conversation`.

## Project layout

```
ui/
  index.html
  vite.config.ts          — dev server + /api proxy to :3000
  tsconfig.json
  vitest.config.ts
  src/
    main.tsx              — entry point, mounts <App />
    App.tsx               — top-level layout, viewport-aware split pane,
                            owns the single useConversation + useSseChatEvents
    api/
      chatClient.ts       — typed wrappers for /api/chat/session/* endpoints
                            (source of truth for the chat REST contract)
      chatStream.ts       — SSE EventSource for chat events
      _fetch.ts           — shared jsonFetch / jsonFetchOrNull / voidFetch
                            helpers
    hooks/
      useConversation.ts  — session lifecycle, sendTurn/confirm/reject/unblock,
                            60s transcript poll, 8s stillThinking flag
      useSseChatEvents.ts — tool-call pill, infra errors, harness progress
                            events, assistant_token_delta streaming buffer
      useViewport.ts      — desktop/tablet/mobile breakpoint
    components/
      ConversationPane.tsx    — left pane, wraps ChatTimeline + ChatComposer
      ChatTimeline.tsx        — renders the full transcript + InFlightAssistantBubble
      AssistantTurnCard.tsx   — assistant turn renderer; hosts ToolCallStatusPill,
                                ConfirmationTurnCard, QuickReplyRow
      UserTurnCard.tsx
      ChatComposer.tsx        — textarea + Send; auto-focus on mount;
                                Shift+Enter newline; whitespace-only Send disabled
      ConfirmationTurnCard.tsx — Accept / Revise buttons, inline
      StructuredCaptureTurnCard.tsx — dense structured-capture form-in-chat
      ToolCallStatusPill.tsx  — 1s threshold timer pill
      StillThinkingIndicator.tsx — >8s whole-turn indicator
      QuickReplyRow.tsx       — suggestion chips
      InfraErrorBanner.tsx    — top-of-pane banner for api-unreachable etc.
      BlockerCard.tsx         — renders when SessionState::Blocked; drives /unblock
      ResultReviewTurnCard.tsx       — post-emit result review card
      SensitivityComparisonCard.tsx  — sensitivity-analysis compare card
      BranchFromHereCard.tsx         — branch-from-here card
      StateInspectorPane.tsx  — right pane: Plan / State / Documents / Jobs /
                                Metrics / History tabs
      DagCanvas.tsx           — xyflow + dagre auto-layout (used by the Plan tab)
      TaskCard.tsx            — DagCanvas node renderer
      ProgressBar.tsx         — task-count summary
      LogViewer.tsx           — kept from legacy; reserved for the right-pane
                                log surface, not currently rendered
      StatusBar.tsx           — kept from legacy; reserved for future reuse
    types/                    — ts-rs generated bindings (regenerate with make types)
    test/                     — Vitest setup + axe-core scan
```

## App.tsx — one session, one SSE

`App.tsx` owns the single `useConversation` and `useSseChatEvents` instances and threads them as props into both panes. This guarantees one session and one `EventSource` for the page; both panes see the same state. Do not re-call these hooks from deeper components.

### Viewport breakpoints (`useViewport`)

- **Desktop (≥1024 px)** — split pane: `ConversationPane` left (~45%), `StateInspectorPane` right (~55%).
- **Tablet / mobile (<1024 px)** — single pane with a `Chat` / `View plan` tab toggle.

### Streaming turn rendering

The conversation service drives `send_turn_streaming` against the Anthropic SSE pipeline. `useSseChatEvents` exposes a `streamingText` buffer fed by `assistant_token_delta` events; `ChatTimeline` renders an `InFlightAssistantBubble` at the bottom of the log with a blinking caret while a turn is in flight. `ConversationPane` calls `sse.resetStreamingText()` when `conv.sending` flips false so the canonical `AssistantTurnCard` takes over seamlessly.

### State Inspector tabs

The 14 tabs are defined in `ui/src/components/state_inspector/index.ts::TABS`. Tab `id` values retain legacy names (`state`, `jobs`, `metrics`, `verifier_decisions`) for routing; the human labels rendered in the UI are in the second column.

| Tab `id` | Label | Source | Purpose |
|---|---|---|---|
| `plan` | Plan | `GET /api/chat/session/:id/dag` | Live DAG via `DagCanvas` |
| `composition` | Composition | `GET /api/chat/session/:id/composition` | Atom-level composer trace (which archetype, which atoms, why) |
| `state` | Status | `GET /api/chat/session/:id/state` | Raw JSON `SessionStateSnapshot` |
| `documents` | Documents | result-card artifacts | Rendered markdown / narrative documents from completed tasks |
| `inputs` | Inputs | `GET /api/chat/session/:id/inputs` | SME-uploaded files; manage `runtime/inputs/` before execution |
| `jobs` | Progress | SSE `harness_progress` | Live feed of harness progress events; auto-switches the first time an event arrives (with aria-live announcement) |
| `metrics` | Performance | `GET /api/chat/session/:id/metrics` | Polled every 4 s while visible; p50/p95/p99 turn latency, Sonnet/Opus split, token totals, cost buckets |
| `figures` | Figures | result-card artifacts | Gallery of every rendered PNG / SVG produced by completed tasks |
| `dashboard` | Dashboard | `GET /api/chat/session/:id/dashboard/index` | Interactive plots from completed tasks (UMAPs, scatter, PCA) |
| `decisions` | Decisions | `runtime/decisions.jsonl` | Typed audit trail of every SME click + LLM-driven mutation |
| `repairs` | Repairs | `runtime/outputs/<task_id>/repairs.json` | Per-task post-hoc repair log: what the agent re-ran, why, and what changed |
| `verifier_decisions` | Verifier | `runtime/claim-verification-report.json` | Narrative-claim cross-check status per completed task |
| `history` | History | session lineage walk | Parent/child SessionTree across the connected component |
| `compare` | Compare | `runtime/cross_version_diff.json` | Row-level diff against a parent or sibling session |

## Server API contract — chat surface

All endpoints are JSON unless noted. The source of truth lives in `crates/server/src/chat_routes/` (per-domain modules under that directory); `ui/src/api/chatClient.ts` is the TypeScript mirror.

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/api/chat/session` | Create a session; returns id + greeting turn. |
| `POST` | `/api/chat/session/:id/turn` | Send a user turn; drives the tool-use loop; returns the assistant turn. |
| `GET` | `/api/chat/session/:id/state` | `SessionStateSnapshot` (state, user_confirmed, task_count, progress). |
| `GET` | `/api/chat/session/:id/transcript` | Full `Vec<Turn>`. |
| `GET` | `/api/chat/session/:id/dag` | In-memory session DAG. |
| `GET` | `/api/chat/session/:id/metrics` | Per-session metrics snapshot. |
| `POST` | `/api/chat/session/:id/confirm` | Deterministic confirm transition (button click → server sets `user_confirmed=true`). |
| `POST` | `/api/chat/session/:id/reject` | Deterministic reject transition. |
| `POST` | `/api/chat/session/:id/unblock` | Deterministic unblock transition for `SessionState::Blocked`. |
| `POST` | `/api/chat/session/:id/progress` | Harness progress event (broadcast over SSE + enqueued in the batcher). |
| `GET` | `/api/chat/session/:id/events` | SSE stream (see below). |

### SSE event types

The UI uses one `EventSource` per session. SSE auto-reconnects; the UI does not need explicit retry logic.

| Event name | Payload (conceptual) |
|---|---|
| `tool_call_started` / `tool_call_finished` | Tool name + duration; feeds `ToolCallStatusPill` |
| `state_advanced` | `SessionStateSnapshot` delta; triggers a re-fetch |
| `harness_progress` | One task lifecycle event; appended to the Jobs tab |
| `infra_error` | Backend infrastructure error; shown in `InfraErrorBanner` |
| `assistant_token_delta` | Streaming text chunk from the in-flight turn |

## Tests

```bash
make test-ui       # cd ui && npx vitest run
```

Authoritative Vitest count in [`.github/ci/expected-test-counts.json::ui_vitest_total`](../.github/ci/expected-test-counts.json) (currently 545). Coverage spans component transitions, per-tab snapshot tests under `state_inspector/`, MetricsTable rendering, and an axe-core a11y scan across every top-level natural-chat component (WCAG 2.1 AA rule set; `color-contrast` disabled in jsdom because canvas isn't available — verified separately in the [accessibility audit](../docs/accessibility-audit.md)).

Playwright e2e lives under `../e2e/`; see [e2e/README.md](../e2e/README.md) for the two-tier strategy.

## Adding a new server endpoint

1. Add the handler in the right per-domain module under `crates/server/src/chat_routes/` (e.g. `tasks/`, `branches/`, or a new module if it doesn't fit) and append its `(method, path)` pair to that module's `pub const ROUTES` table; register the handler on the Router via `mod.rs`.
2. Add a typed wrapper in `ui/src/api/chatClient.ts`.
3. If the endpoint streams, add the event variant to `chatStream.ts` and dispatch it from `useSseChatEvents`.
4. Call it from the relevant hook or component.

ts-rs types are regenerated via `make types`; don't hand-edit files in `ui/src/types/`.

## Building for production

```bash
make ui          # runs make types, then npm install && npx vite build
```

Output lands in `ui/dist/`. The server serves this directory statically via `tower_http::services::ServeDir` (wired in `crates/server/src/main.rs`). For deploy, run `ecaa-workflow serve --port 3000` with `ui/dist/` present.
