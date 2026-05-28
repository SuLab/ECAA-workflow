import { act, renderHook } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { useAsync } from './useAsync'

describe('useAsync — async-lifecycle hook', () => {
  it('captures the resolved value + clears busy + error on a happy path', async () => {
    const { result } = renderHook(() => useAsync())
    expect(result.current.busy).toBe(false)
    expect(result.current.error).toBeNull()

    let returned: number | undefined
    await act(async () => {
      returned = await result.current.run(async () => 42)
    })

    expect(returned).toBe(42)
    expect(result.current.busy).toBe(false)
    expect(result.current.error).toBeNull()
  })

  it('captures the error message + flips busy false on the error path', async () => {
    const { result } = renderHook(() => useAsync())

    await act(async () => {
      await result.current.run(async () => {
        throw new Error('boom')
      })
    })

    expect(result.current.busy).toBe(false)
    expect(result.current.error).toBe('boom')

    // clearError resets without firing a new call.
    act(() => {
      result.current.clearError()
    })
    expect(result.current.error).toBeNull()
  })
})
