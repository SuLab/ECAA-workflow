// Test helper that wraps a UI under ThemeProvider with an optional
// seeded preference. Mirrors the production render path in main.tsx so
// components that call useTheme() work inside RTL's `render`.
//
// Usage:
// import { renderWithTheme } from './renderWithTheme'
// const { container } = renderWithTheme(<MyComponent />, { mode: 'dark' })
//
// A test that wants both variants can iterate:
// describe.each(['light', 'dark'] as const)('%s mode', (mode) => {
// it('...', () => { renderWithTheme(<X />, { mode }) })
// })

import { render, type RenderOptions, type RenderResult } from '@testing-library/react'
import type { ReactElement } from 'react'
import { ThemeProvider } from '../hooks/useTheme'

export interface ThemeRenderOptions extends Omit<RenderOptions, 'wrapper'> {
  mode?: 'light' | 'dark' | 'system'
}

export function renderWithTheme(
  ui: ReactElement,
  options: ThemeRenderOptions = {},
): RenderResult {
  const { mode, ...rest } = options
  // Seed the stored preference before rendering so the provider's
  // initial-state read resolves to the requested mode. The FOUC script
  // isn't loaded in jsdom, so we set data-theme explicitly.
  if (mode) {
    window.localStorage.setItem('swfc.theme', mode)
    if (mode !== 'system') {
      document.documentElement.dataset.theme = mode
    }
  } else {
    window.localStorage.removeItem('swfc.theme')
    delete document.documentElement.dataset.theme
  }
  return render(ui, { wrapper: ThemeProvider, ...rest })
}
