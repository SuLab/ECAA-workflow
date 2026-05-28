// Shared modal-dialog chrome.
//
// Wraps the recurring `role="dialog" aria-modal="true"` + backdrop +
// Escape-handler + focus-trap pattern that 8 components in the
// codebase reimplemented from scratch. Consumers supply the inner
// content; this primitive takes care of:
//
// - The backdrop element (full-screen translucent overlay) at the
// palette-defined Z-index tier.
// - The dialog container with `role="dialog"` / `aria-modal`
// wiring + the supplied aria-label / aria-labelledby.
// - Escape-key handler that calls `onClose`.
// - Optional outside-click handler (`closeOnOutsideClick`, default
// true) that calls `onClose` when the SME mouses down on the
// backdrop, not the dialog body.
// - Focus management: focuses the first focusable element on mount
// and traps Tab/Shift+Tab cycling so keyboard users can't escape
// the dialog without explicitly closing it.
//
// Sizing / styling stay caller-driven via the `contentStyle` prop so
// the small ShareModal-style centered card and the large drawer-style
// edge-anchored panels can share the same primitive.

import { useCallback, useEffect, useRef, type CSSProperties, type ReactNode } from 'react'
import { Z } from '../../lib/z-index'

interface DialogProps {
  /** Called on Escape, outside-click (if enabled), and close-button click. */
  onClose: () => void
  /** Visible label or anchor reference; one of the two is required for a11y. */
  ariaLabel?: string
  ariaLabelledby?: string
  /**
   * When true (default), mousedown on the backdrop fires `onClose`.
   * Set false for high-stakes confirms (e.g. ClinicalConfirmGate)
   * where an errant click shouldn't be a dismissal.
   */
  closeOnOutsideClick?: boolean
  /** Z-index tier. Defaults to `Z.MODAL`; lift higher for nested dialogs. */
  zIndex?: number
  /** Optional override of the backdrop styling (e.g. drawer-style flex layout). */
  backdropStyle?: CSSProperties
  /** Per-dialog style extensions merged over the default centered-card. */
  contentStyle?: CSSProperties
  children: ReactNode
}

/**
 * Selector for elements considered focusable inside the trap. Mirrors
 * the standard "tabbable elements" list axe-core uses.
 */
const FOCUSABLE_SELECTOR = [
  'a[href]',
  'area[href]',
  'button:not([disabled])',
  'input:not([disabled]):not([type="hidden"])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  '[contenteditable="true"]',
  '[tabindex]:not([tabindex="-1"])',
].join(',')

function focusableInside(container: HTMLElement): HTMLElement[] {
  return Array.from(container.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR))
    .filter((el) => !el.hasAttribute('disabled'))
}

export function Dialog({
  onClose,
  ariaLabel,
  ariaLabelledby,
  closeOnOutsideClick = true,
  zIndex = Z.MODAL,
  backdropStyle,
  contentStyle,
  children,
}: DialogProps): JSX.Element {
  const contentRef = useRef<HTMLDivElement | null>(null)
  // Cache the previously-focused element so we can restore focus on
  // close — without this, dismissing the dialog lands focus on
  // document.body and keyboard users lose their place.
  const previousFocus = useRef<HTMLElement | null>(null)

  // ── Escape closes the dialog ─────────────────────────────────────────
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault()
        onClose()
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose])

  // ── Focus management on mount/unmount ────────────────────────────────
  useEffect(() => {
    previousFocus.current =
      typeof document !== 'undefined' && document.activeElement instanceof HTMLElement
        ? (document.activeElement as HTMLElement)
        : null

    // Focus the first focusable element after the browser has painted
    // the dialog. Falls back to the dialog container itself when the
    // body has no focusable descendants.
    const id = requestAnimationFrame(() => {
      const container = contentRef.current
      if (!container) return
      const items = focusableInside(container)
      if (items.length > 0) {
        items[0]?.focus()
      } else {
        container.focus()
      }
    })

    return () => {
      cancelAnimationFrame(id)
      // Restore focus to the element that owned it before the dialog
      // opened. Guarded — the element may have unmounted while the
      // dialog was open.
      const prev = previousFocus.current
      if (prev && typeof prev.focus === 'function' && document.body.contains(prev)) {
        prev.focus()
      }
    }
  }, [])

  // ── Focus trap: cycle Tab / Shift+Tab inside the dialog ──────────────
  const onKeyDown = useCallback((e: React.KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== 'Tab') return
    const container = contentRef.current
    if (!container) return
    const items = focusableInside(container)
    if (items.length === 0) {
      e.preventDefault()
      container.focus()
      return
    }
    const first = items[0]
    const last = items[items.length - 1]
    const active = document.activeElement as HTMLElement | null
    if (e.shiftKey) {
      // Shift+Tab from the first item wraps to the last.
      if (active === first || !container.contains(active)) {
        e.preventDefault()
        last?.focus()
      }
    } else if (active === last) {
      // Tab from the last item wraps to the first.
      e.preventDefault()
      first?.focus()
    }
  }, [])

  const onMouseDown = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      if (!closeOnOutsideClick) return
      if (e.target === e.currentTarget) {
        onClose()
      }
    },
    [closeOnOutsideClick, onClose],
  )

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label={ariaLabel}
      aria-labelledby={ariaLabelledby}
      onMouseDown={onMouseDown}
      onKeyDown={onKeyDown}
      style={{
        position: 'fixed',
        inset: 0,
        background: 'rgba(0,0,0,0.35)',
        zIndex,
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        ...backdropStyle,
      }}
    >
      <div
        ref={contentRef}
        tabIndex={-1}
        onMouseDown={(e) => e.stopPropagation()}
        style={{
          background: 'var(--color-surface-0)',
          border: '1px solid var(--color-border-default)',
          borderRadius: 8,
          padding: '1rem',
          boxShadow: '0 24px 60px rgba(0,0,0,0.3)',
          outline: 'none',
          ...contentStyle,
        }}
      >
        {children}
      </div>
    </div>
  )
}
