import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import {
  applySettings,
  DEFAULT_SETTINGS,
  loadSettings,
  saveSettings,
} from './a11y'

beforeEach(() => {
  window.localStorage.clear()
  const root = document.documentElement
  root.style.removeProperty('--a11y-font-scale')
  delete root.dataset.a11yHighContrast
  delete root.dataset.a11yReducedMotion
  delete root.dataset.a11yColorSafe
})

afterEach(() => {
  window.localStorage.clear()
})

describe('a11y settings', () => {
  it('defaults when nothing is stored', () => {
    expect(loadSettings()).toEqual(DEFAULT_SETTINGS)
  })

  it('round-trips via save/load', () => {
    const s = { fontScale: 1.2, highContrast: true, reducedMotion: true, colorSafe: false }
    saveSettings(s)
    expect(loadSettings()).toEqual(s)
  })

  it('rejects out-of-range font scale', () => {
    window.localStorage.setItem('swfc.a11y.fontScale', '4')
    expect(loadSettings().fontScale).toBe(1.0)
  })

  it('applies CSS variable + data attributes to <html>', () => {
    applySettings({
      fontScale: 1.25,
      highContrast: true,
      reducedMotion: true,
      colorSafe: true,
    })
    const root = document.documentElement
    expect(root.style.getPropertyValue('--a11y-font-scale')).toBe('1.25')
    expect(root.dataset.a11yHighContrast).toBe('1')
    expect(root.dataset.a11yReducedMotion).toBe('1')
    expect(root.dataset.a11yColorSafe).toBe('1')
  })

  it('clears data attributes when prefs are off', () => {
    applySettings({
      fontScale: 1.0,
      highContrast: false,
      reducedMotion: false,
      colorSafe: false,
    })
    const root = document.documentElement
    expect(root.dataset.a11yHighContrast).toBeUndefined()
    expect(root.dataset.a11yReducedMotion).toBeUndefined()
    expect(root.dataset.a11yColorSafe).toBeUndefined()
  })
})
