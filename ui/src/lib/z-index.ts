/**
 * Z-index tier system.
 *
 * Defines a small set of named tiers so layered UI picks a tier rather
 * than an ad-hoc number. Ad-hoc values across components (10, 30, 40,
 * 50, 60, 80, 100, 1000) cause collisions; consume one of the tiers
 * below instead.
 */

export const Z = {
  /** Below-content layer (rare; for backdrops behind plot canvases). */
  BACKDROP: -1,

  /** Default content layer. Don't set z-index for normal flow. */
  CONTENT: 0,

  /** Sticky banners / headers attached to content. */
  STICKY_BANNER: 10,

  /** Popovers anchored to inline triggers (tooltips, ExplainButton output). */
  ANCHORED_POPOVER: 30,

  /** Dropdown menus, side-panel drawers. */
  DROPDOWN: 40,

  /** Toasts, transient notifications. */
  TOAST: 50,

  /** Nested drawers (one drawer opens another). */
  NESTED_DRAWER: 60,

  /** Modal dialogs (Share, Settings) — above drawers, below palette. */
  MODAL: 80,

  /** Command palette + lightboxes — covers everything except the literature
   * popover which intentionally sits on top of the palette. */
  PALETTE: 100,

  /** Highest layer: literature-context popovers + edge-proof drawer. */
  TOP: 1000,
} as const;
