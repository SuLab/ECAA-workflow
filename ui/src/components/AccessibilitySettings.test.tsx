// button aria label, dropdown reveal, High-contrast checkbox wiring to
// `swfc.a11y.highContrast` + document.documentElement.dataset, and
// Escape key dismissal.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import AccessibilitySettings from './AccessibilitySettings'

beforeEach(() => {
  window.localStorage.clear()
  delete document.documentElement.dataset.a11yHighContrast
  delete document.documentElement.dataset.a11yReducedMotion
  delete document.documentElement.dataset.a11yColorSafe
})

afterEach(() => {
  vi.restoreAllMocks()
  window.localStorage.clear()
  delete document.documentElement.dataset.a11yHighContrast
  delete document.documentElement.dataset.a11yReducedMotion
  delete document.documentElement.dataset.a11yColorSafe
})

describe('AccessibilitySettings', () => {
  it('renders a gear button with aria-label "Accessibility settings"', () => {
    render(<AccessibilitySettings />)
    expect(screen.getByLabelText('Accessibility settings')).toBeInTheDocument()
  })

  it('opens the dropdown and reveals the four controls', () => {
    render(<AccessibilitySettings />)
    fireEvent.click(screen.getByLabelText('Accessibility settings'))
    expect(screen.getByRole('dialog')).toBeInTheDocument()
    expect(screen.getByText(/Font size/)).toBeInTheDocument()
    expect(screen.getByText('High contrast')).toBeInTheDocument()
    expect(screen.getByText('Reduce motion')).toBeInTheDocument()
    expect(screen.getByText('Color-blind-safe palette')).toBeInTheDocument()
  })

  it('toggling High contrast writes swfc.a11y.highContrast + dataset flag', async () => {
    render(<AccessibilitySettings />)
    fireEvent.click(screen.getByLabelText('Accessibility settings'))
    const label = screen.getByText('High contrast').closest('label')!
    const checkbox = label.querySelector('input[type="checkbox"]') as HTMLInputElement
    fireEvent.click(checkbox)
    await waitFor(() =>
      expect(window.localStorage.getItem('swfc.a11y.highContrast')).toBe('1'),
    )
    expect(document.documentElement.dataset.a11yHighContrast).toBe('1')
  })

  it('Escape closes the dropdown', async () => {
    render(<AccessibilitySettings />)
    fireEvent.click(screen.getByLabelText('Accessibility settings'))
    expect(screen.getByRole('dialog')).toBeInTheDocument()
    fireEvent.keyDown(document, { key: 'Escape' })
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull())
  })
})
