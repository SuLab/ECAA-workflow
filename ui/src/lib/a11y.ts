// Accessibility preference persistence + application.
//
// Applied globally on mount and on every user toggle via CSS variables
// on <html>. The AccessibilitySettings component reads / writes this.
// All four prefs default to "off" so existing users don't see layout
// shifts after upgrade.

const KEYS = {
  fontScale: 'swfc.a11y.fontScale',
  highContrast: 'swfc.a11y.highContrast',
  reducedMotion: 'swfc.a11y.reducedMotion',
  colorSafe: 'swfc.a11y.colorSafe',
} as const

export interface A11ySettings {
  /** Relative CSS font-size scale, 0.8–1.4. Default 1.0. */
  fontScale: number
  highContrast: boolean
  reducedMotion: boolean
  colorSafe: boolean
}

export const DEFAULT_SETTINGS: A11ySettings = {
  fontScale: 1.0,
  highContrast: false,
  reducedMotion: false,
  colorSafe: false,
}

export function loadSettings(): A11ySettings {
  if (typeof window === 'undefined') return { ...DEFAULT_SETTINGS }
  try {
    const fs = parseFloat(window.localStorage.getItem(KEYS.fontScale) ?? '')
    return {
      fontScale: Number.isFinite(fs) && fs >= 0.8 && fs <= 1.4 ? fs : 1.0,
      highContrast: window.localStorage.getItem(KEYS.highContrast) === '1',
      reducedMotion: window.localStorage.getItem(KEYS.reducedMotion) === '1',
      colorSafe: window.localStorage.getItem(KEYS.colorSafe) === '1',
    }
  } catch {
    return { ...DEFAULT_SETTINGS }
  }
}

export function saveSettings(s: A11ySettings): void {
  try {
    window.localStorage.setItem(KEYS.fontScale, s.fontScale.toString())
    window.localStorage.setItem(KEYS.highContrast, s.highContrast ? '1' : '0')
    window.localStorage.setItem(KEYS.reducedMotion, s.reducedMotion ? '1' : '0')
    window.localStorage.setItem(KEYS.colorSafe, s.colorSafe ? '1' : '0')
  } catch {
    // ignore
  }
}

/**
 * Push prefs onto the `:root` element so global CSS variables pick them
 * up. Consumers can also read the data-* attributes for style branches.
 */
export function applySettings(s: A11ySettings): void {
  if (typeof document === 'undefined') return
  const root = document.documentElement
  root.style.setProperty('--a11y-font-scale', s.fontScale.toString())
  if (s.highContrast) root.dataset.a11yHighContrast = '1'
  else delete root.dataset.a11yHighContrast
  if (s.reducedMotion) root.dataset.a11yReducedMotion = '1'
  else delete root.dataset.a11yReducedMotion
  if (s.colorSafe) root.dataset.a11yColorSafe = '1'
  else delete root.dataset.a11yColorSafe
}
