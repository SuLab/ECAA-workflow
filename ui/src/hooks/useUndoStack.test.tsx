// Coverage: push semantics (single token, latest wins), 30s window
// auto-expiry, manual clear, throwing accessor outside provider.

import { act, renderHook } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { UndoStackProvider, useUndoStack, UNDO_WINDOW_MS } from './useUndoStack'

function wrapper({ children }: { children: React.ReactNode }) {
  return <UndoStackProvider>{children}</UndoStackProvider>
}

beforeEach(() => {
  vi.useFakeTimers()
})

afterEach(() => {
  vi.useRealTimers()
})

describe('useUndoStack', () => {
  it('push stores a token and exposes it on the context value', () => {
    const undo = vi.fn(async () => {})
    const { result } = renderHook(() => useUndoStack(), { wrapper })
    act(() => {
      result.current.push({ kind: 'amend', label: 'amend X', undo })
    })
    expect(result.current.token?.kind).toBe('amend')
    expect(result.current.token?.label).toBe('amend X')
  })

  it('push overwrites the prior token (single-slot semantics)', () => {
    const undo1 = vi.fn(async () => {})
    const undo2 = vi.fn(async () => {})
    const { result } = renderHook(() => useUndoStack(), { wrapper })
    act(() => {
      result.current.push({ kind: 'amend', label: 'first', undo: undo1 })
    })
    act(() => {
      result.current.push({ kind: 'rerun', label: 'second', undo: undo2 })
    })
    expect(result.current.token?.label).toBe('second')
    expect(result.current.token?.kind).toBe('rerun')
  })

  it('token auto-expires after UNDO_WINDOW_MS', () => {
    const { result } = renderHook(() => useUndoStack(), { wrapper })
    act(() => {
      result.current.push({ kind: 'branch', label: 'branch X', undo: async () => {} })
    })
    expect(result.current.token).not.toBeNull()
    act(() => {
      vi.advanceTimersByTime(UNDO_WINDOW_MS + 100)
    })
    expect(result.current.token).toBeNull()
  })

  it('clear() removes the token immediately', () => {
    const { result } = renderHook(() => useUndoStack(), { wrapper })
    act(() => {
      result.current.push({ kind: 'amend', label: 'X', undo: async () => {} })
    })
    expect(result.current.token).not.toBeNull()
    act(() => {
      result.current.clear()
    })
    expect(result.current.token).toBeNull()
  })

  it('throws when used outside UndoStackProvider', () => {
    expect(() => renderHook(() => useUndoStack())).toThrow(/UndoStackProvider/)
  })
})
