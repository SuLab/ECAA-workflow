import { act, renderHook } from '@testing-library/react'
import { afterEach, describe, expect, it } from 'vitest'
import { ThemeProvider, useTheme } from './useTheme'

const STORAGE_KEY = 'swfc.theme'

afterEach(() => {
  window.localStorage.removeItem(STORAGE_KEY)
  delete document.documentElement.dataset.theme
})

describe('useTheme', () => {
  it('defaults to system preference when no stored value is present', () => {
    const { result } = renderHook(() => useTheme(), { wrapper: ThemeProvider })
    expect(result.current.preference).toBe('system')
    // jsdom's matchMedia stub resolves to false (see test/setup.ts),
    // so system resolves to light.
    expect(result.current.mode).toBe('light')
  })

  it('reflects mode onto <html data-theme> on mount', () => {
    renderHook(() => useTheme(), { wrapper: ThemeProvider })
    expect(document.documentElement.dataset.theme).toBe('light')
  })

  it('setPreference("dark") persists and flips data-theme', () => {
    const { result } = renderHook(() => useTheme(), { wrapper: ThemeProvider })
    act(() => {
      result.current.setPreference('dark')
    })
    expect(result.current.preference).toBe('dark')
    expect(result.current.mode).toBe('dark')
    expect(document.documentElement.dataset.theme).toBe('dark')
    expect(window.localStorage.getItem(STORAGE_KEY)).toBe('dark')
  })

  it('setPreference("light") persists and flips data-theme', () => {
    window.localStorage.setItem(STORAGE_KEY, 'dark')
    const { result } = renderHook(() => useTheme(), { wrapper: ThemeProvider })
    act(() => {
      result.current.setPreference('light')
    })
    expect(result.current.mode).toBe('light')
    expect(document.documentElement.dataset.theme).toBe('light')
    expect(window.localStorage.getItem(STORAGE_KEY)).toBe('light')
  })

  it('exposes the token record for the active mode', () => {
    const { result } = renderHook(() => useTheme(), { wrapper: ThemeProvider })
    expect(result.current.tokens.surface0).toBeTruthy()
    expect(result.current.tokens.accent).toBeTruthy()
    // Chart palette has 10 entries matching the CSS var set.
    expect(result.current.tokens.chart).toHaveLength(10)
  })

  it('picks up a stored preference on initial render', () => {
    window.localStorage.setItem(STORAGE_KEY, 'dark')
    const { result } = renderHook(() => useTheme(), { wrapper: ThemeProvider })
    expect(result.current.preference).toBe('dark')
    expect(result.current.mode).toBe('dark')
  })

  it('throws when called outside <ThemeProvider>', () => {
    // renderHook without a wrapper — the hook should throw on mount.
    expect(() => renderHook(() => useTheme())).toThrow(/ThemeProvider/)
  })
})
