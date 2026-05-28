import { act, renderHook } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { useViewport } from './useViewport'

describe('useViewport', () => {
  it('reports current window dimensions on mount', () => {
    const { result } = renderHook(() => useViewport())
    expect(result.current.width).toBe(window.innerWidth)
    expect(result.current.height).toBe(window.innerHeight)
  })

  it('updates on resize events', () => {
    const { result } = renderHook(() => useViewport())
    const before = result.current.width
    act(() => {
      Object.defineProperty(window, 'innerWidth', { value: 800, configurable: true })
      Object.defineProperty(window, 'innerHeight', { value: 600, configurable: true })
      window.dispatchEvent(new Event('resize'))
    })
    expect(result.current.width).toBe(800)
    expect(result.current.height).toBe(600)
    expect(result.current.width).not.toBe(before)
  })
})
