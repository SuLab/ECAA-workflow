// Central polling-cadence constants. Lives in lib/ so non-component
// modules (api/, hooks/) can import without pulling component-tree
// dependencies. Values match what each call site historically used.

/** 60s transcript reconciliation poll — fallback for missed turn_appended SSE events. */
export const TRANSCRIPT_POLL_MS = 60_000

/** 4s metrics refresh while the Performance tab is visible. */
export const METRICS_POLL_MS = 4_000

/** 30s pilot report refresh while the Performance tab is visible. Pilot data is static once written. */
export const PILOT_POLL_MS = 30_000

/** 2s progress.log tail while a task is running. */
export const LOG_POLL_MS = 2_000

/** 5s figures-manifest refresh while the figures tab of the log drawer is open. */
export const FIGURES_POLL_MS = 5_000

/** 30s git-status / git-log refresh while the settings page is open. */
export const GIT_STATUS_POLL_MS = 30_000

/** 3s execution status poll while the harness is running. */
export const EXECUTION_POLL_MS = 3_000

/** 2s active-tasks poll for the per-task progress panel. Same cadence
 * as the per-task progress.log tail so the heartbeat dot, elapsed time,
 * and last-line readout all refresh together. */
export const ACTIVE_TASKS_POLL_MS = 2_000

/** 15s budget-chip refresh — header-only, non-blocking. */
export const BUDGET_POLL_MS = 15_000

/** 30s browser-notification permission state refresh. */
export const NOTIF_PERMISSION_POLL_MS = 30_000

/** 15s dashboard-index refresh while the Dashboard tab is visible. */
export const DASHBOARD_INDEX_POLL_MS = 15_000

/** 8s threshold before showing "Still thinking…" indicator on a turn. */
export const STILL_THINKING_MS = 8_000

/** D8 mitigation — escalation cadence for the in-flight-turn indicator.
 * Each threshold maps to a `ThinkingStage` value the chip renders against.
 * Selected to match the Anthropic-side stall failure modes the field
 * report turned up: most turns complete in <8s; legitimately slow turns
 * sit between 8s and 30s; >30s usually means a 4xx schema reject is
 * imminent or an Anthropic-side queue stall; >60s the SME should be
 * told to consider refreshing; >=90s the only reasonable next step is to
 * cancel and resubmit. The threshold ladder matches the server-side
 * `ECAA_ANTHROPIC_TIMEOUT_SECS=180` ceiling so a true backend timeout
 * lands while the user is already in the cancelable stage. */
export const SLOW_THINKING_MS = 30_000
export const VERY_SLOW_THINKING_MS = 60_000
export const CANCELABLE_THINKING_MS = 90_000

/** 500ms tick for the undo-toast countdown bar. */
export const UNDO_TOAST_TICK_MS = 500

/** Poll for session-tree refresh (matches BUDGET_POLL_MS and DASHBOARD_INDEX_POLL_MS at 15s). */
export const SESSION_TREE_POLL_MS = 15_000

/** Poll cadence for the stuck-tasks banner. Distinct cohort from the 30s
 * pilot/git-status polls — kept separate so a banner-cadence change doesn't
 * accidentally slow infrastructure polls. */
export const STUCK_TASKS_POLL_MS = 30_000

/** Recent sessions dropdown refresh cadence — 30s catches branches created
 * in other tabs without flooding the index endpoint. */
export const RECENT_SESSIONS_POLL_MS = 30_000

/** Task ops panel refresh cadence (while a task is running). */
export const TASK_OPS_POLL_MS = 30_000

/** Repair-proposals poll cadence. */
export const REPAIR_PROPOSALS_POLL_MS = 4_000

/** Background tick rate for elapsed-time displays in RunningTasksPanel etc. */
export const ELAPSED_TIME_TICK_MS = 1_000

/** Scroll-to-bottom debounce after streaming output settles. */
export const SCROLL_DEBOUNCE_MS = 500

/** DagCanvas node-hover clear debounce — lets the cursor "rest" on a node. */
export const NODE_HOVER_DEBOUNCE_MS = 250

/** Title-bar blink interval for fallback (non-Notification-API) blocker alerts. */
export const TITLE_BLINK_INTERVAL_MS = 1_000

/** "Just now" relative-time cutoff: differences under this render as "just now". */
export const JUST_NOW_THRESHOLD_MS = 30_000

/** Background tick for `useRelativeTime()` so visible timestamps advance. */
export const RELATIVE_TIME_TICK_MS = 60_000

/**
 * Random delay (0..maxJitterMs) used to stagger mount-time poll bursts
 * so 3+ components opening at the same time don't fire their first
 * fetch in the same frame. Math.random() is fine — perfect uniformity
 * isn't required, and the spread is purely a network-courtesy.
 */
export function jitterMs(maxJitterMs = 1500): number {
  return Math.floor(Math.random() * maxJitterMs)
}
