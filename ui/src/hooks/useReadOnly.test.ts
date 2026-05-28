import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import { isReadOnly } from './useReadOnly'

const originalLocation = window.location

afterEach(() => {
  Object.defineProperty(window, 'location', {
    writable: true,
    value: originalLocation,
  })
})

function setSearch(search: string) {
  Object.defineProperty(window, 'location', {
    writable: true,
    value: { ...originalLocation, search } as Location,
  })
}

beforeEach(() => {
  setSearch('')
})

describe('isReadOnly', () => {
  it('returns false when ?share_token= is absent', () => {
    setSearch('?session=abc')
    expect(isReadOnly()).toBe(false)
  })

  it('returns true when ?share_token= is present', () => {
    setSearch('?share_token=tok')
    expect(isReadOnly()).toBe(true)
  })

  it('returns false when share_token is empty (length 0 is treated as absent)', () => {
    setSearch('?share_token=')
    expect(isReadOnly()).toBe(false)
  })
})
