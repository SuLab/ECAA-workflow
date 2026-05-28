/**
 * Single source of truth for every selector the suite uses.
 *
 * Each entry points at an existing ARIA role or aria-label in the UI.
 * We deliberately do NOT add data-testid attributes to the UI. The
 * accessibility audit established that every user-visible surface is
 * addressable via semantic roles; the test suite leans on that instead
 * of duplicating the information with test-only hooks.
 */

import type { TabKind } from './types'

export const sel = {
  // ── App shell ───────────────────────────────────────────────────────────
  titleBar: 'text=ECAA-workflow',
  mobileTablist: '[role="tablist"][aria-label="Mobile view switcher"]',
  mobileTab: (v: 'chat' | 'state') =>
    `[role="tablist"][aria-label="Mobile view switcher"] [role="tab"]:has-text("${
      v === 'chat' ? 'Chat' : 'View plan'
    }")`,
  chatPaneWrapper: '[aria-label="Chat pane"]',
  statePaneWrapper: '[aria-label="State inspector pane"]',

  // ── ConversationPane ────────────────────────────────────────────────────
  chatLog: '[role="log"][aria-live="polite"][aria-relevant="additions text"]',
  composer: '[aria-label="Message"]',
  sendButton: '[aria-label="Send message"]',
  userBubble: '[aria-label="Your message"]',
  assistantBubble: '[aria-label="Assistant message"]',
  streamingBubble: '[aria-label="Assistant message (streaming)"]',

  // ── ConfirmationTurnCard ────────────────────────────────────────────────
  confirmationCard: '[aria-label="Plan summary — your confirmation"]',
  confirmButton:
    '[aria-label="Plan summary — your confirmation"] button:has-text("Accept")',
  rejectButton:
    '[aria-label="Plan summary — your confirmation"] button:has-text("Revise")',

  // ── BlockerCard ─────────────────────────────────────────────────────────
  // Match by prefix so the selector addresses every BlockerCard
  // regardless of its typed kind, and still excludes InfraErrorBanner
  // (which has no aria-label).
  blockerCard: '[role="alert"][aria-label^="Conversation blocked"]',
  // Target the BlockerCard's primary action by structural position:
  // direct child of the alert section. The card embeds an ExplainButton
  // (a nested <button> inside the reason <p>) which would otherwise
  // also match a generic `... button` selector and trip strict mode.
  // The structured-decision path and the default unblock path both
  // render their button as a direct child of the section, so `> button`
  // matches the right element regardless of which path renders. Stall
  // recovery uses StallActionButtons (own aria-label) and is targeted
  // separately when those tests need it.
  blockerUnblockButton:
    '[role="alert"][aria-label^="Conversation blocked"] > button',

  // ── InfraErrorBanner ────────────────────────────────────────────────────
  // InfraErrorBanner uses role=alert too, but has no aria-label. We distinguish
  // by excluding the blocker's aria-label and by the dismiss button.
  infraBanner: '[role="alert"][aria-live="assertive"]',
  infraDismissButton: '[aria-label="Dismiss notification"]',

  // ── ToolCallStatusPill vs StillThinkingIndicator ───────────────────────
  // Both use role=status / aria-live=polite. The pill lives inside an
  // AssistantTurnCard; the still-thinking indicator lives directly inside
  // the ConversationPane between the log and the composer. Disambiguate
  // by parent and by text.
  toolPillInLatestAssistant:
    '[aria-label="Assistant message"] [role="status"]',
  stillThinking: '[role="status"]:has-text("Still thinking")',

  // ── QuickReplyRow ───────────────────────────────────────────────────────
  quickReplyButton: (label: string) =>
    `[aria-label="Assistant message"] button:has-text("${label}")`,

  // ── StateInspectorPane ──────────────────────────────────────────────────
  inspectorTablist: '[role="tablist"][aria-label="State inspector"]',
  inspectorTab: (k: TabKind) => `#state-tab-${k}`,
  inspectorPanel: (k: TabKind) => `#state-panel-${k}`,
  stateBadge: '[aria-label="Session state"]',
  jobsBadge: (count: number) => `[aria-label="${count} progress events"]`,
  metricsTable: '[aria-label="Per-session metrics"]',
  jobsFeed: '[aria-label="Harness progress feed"]',
} as const
