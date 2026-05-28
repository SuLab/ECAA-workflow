// Unit tests for the shared fetch wrappers in _fetch.ts. Covers the
// empty-body error-message fallback (the vite-proxy-misconfigured UX
// regression that motivated this file), the non-empty body
// preservation, and the 404-as-null short-circuit in jsonFetchOrNull.
//
// The wrappers also attach a bearer token from
// `window.__ECAA_AUTH_TOKEN__`; the auth-header tests at the bottom
// of this file cover that path.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
  ApiClientError,
  FetchError,
  isApiError,
  jsonFetch,
  jsonFetchOrNull,
  jsonFetchRaw,
  voidFetch,
} from './_fetch'

afterEach(() => {
  vi.unstubAllGlobals()
  // Clear the bootstrap-injected auth token between tests.
  if (typeof window !== 'undefined') {
    delete (window as unknown as { __ECAA_AUTH_TOKEN__?: string }).__ECAA_AUTH_TOKEN__
  }
})

describe('jsonFetch', () => {
  it('throws with URL + status + statusText when body is empty', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response('', { status: 500, statusText: 'Internal Server Error' }),
      ),
    )
    await expect(jsonFetch('/api/foo')).rejects.toThrow(
      /500 Internal Server Error from \/api\/foo/,
    )
  })

  it('throws with body text when body is non-empty', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response('boom: classifier failed', {
          status: 500,
          statusText: 'Internal Server Error',
        }),
      ),
    )
    await expect(jsonFetch('/api/foo')).rejects.toThrow('boom: classifier failed')
  })
})

describe('voidFetch', () => {
  it('throws with URL + status + statusText when body is empty', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response('', { status: 500, statusText: 'Internal Server Error' }),
      ),
    )
    await expect(voidFetch('/api/bar')).rejects.toThrow(
      /500 Internal Server Error from \/api\/bar/,
    )
  })

  it('throws with body text when body is non-empty', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response('rejected: bad payload', {
          status: 400,
          statusText: 'Bad Request',
        }),
      ),
    )
    await expect(voidFetch('/api/bar')).rejects.toThrow('rejected: bad payload')
  })
})

describe('jsonFetchOrNull', () => {
  it('returns null on 404 with empty body', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response('', { status: 404, statusText: 'Not Found' }),
      ),
    )
    await expect(jsonFetchOrNull('/api/baz')).resolves.toBeNull()
  })

  it('returns null on 404 with non-empty body', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response('not here', { status: 404, statusText: 'Not Found' }),
      ),
    )
    await expect(jsonFetchOrNull('/api/baz')).resolves.toBeNull()
  })

  it('returns null on 204 NO_CONTENT', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response(null, { status: 204, statusText: 'No Content' }),
      ),
    )
    await expect(jsonFetchOrNull('/api/baz')).resolves.toBeNull()
  })
})

describe('jsonFetch auth header', () => {
  beforeEach(() => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response('{}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        }),
      ),
    )
  })

  it('attaches Authorization: Bearer from window.__ECAA_AUTH_TOKEN__', async () => {
    (window as unknown as { __ECAA_AUTH_TOKEN__?: string }).__ECAA_AUTH_TOKEN__ =
      'tok-123'
    await jsonFetch('/api/health', {})
    const call = (globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls[0]!
    const headers = new Headers(call[1].headers)
    expect(headers.get('authorization')).toBe('Bearer tok-123')
  })

  it('omits Authorization when token is absent', async () => {
    delete (window as unknown as { __ECAA_AUTH_TOKEN__?: string }).__ECAA_AUTH_TOKEN__
    await jsonFetch('/api/health', {})
    const call = (globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls[0]!
    const headers = new Headers(call[1].headers)
    expect(headers.get('authorization')).toBe(null)
  })

  it('also attaches Authorization on voidFetch', async () => {
    (window as unknown as { __ECAA_AUTH_TOKEN__?: string }).__ECAA_AUTH_TOKEN__ =
      'tok-456'
    await voidFetch('/api/health', {})
    const call = (globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls[0]!
    const headers = new Headers(call[1].headers)
    expect(headers.get('authorization')).toBe('Bearer tok-456')
  })
})

// ── Typed error envelope (C25) ─────────────────────────────────────────

describe('isApiError', () => {
  it('accepts `{error: {code, message}}` envelopes', () => {
    expect(
      isApiError({ error: { code: 'session_not_found', message: 'no such session' } }),
    ).toBe(true)
  })

  it('rejects raw strings', () => {
    expect(isApiError('boom')).toBe(false)
  })

  it('rejects partial envelopes (missing message)', () => {
    expect(isApiError({ error: { code: 'foo' } })).toBe(false)
  })

  it('rejects partial envelopes (non-string code)', () => {
    expect(isApiError({ error: { code: 42, message: 'x' } })).toBe(false)
  })

  it('rejects null + undefined + primitives', () => {
    expect(isApiError(null)).toBe(false)
    expect(isApiError(undefined)).toBe(false)
    expect(isApiError(42)).toBe(false)
  })
})

describe('jsonFetch — typed ApiClientError', () => {
  it('throws ApiClientError when body matches the typed envelope', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response(
          JSON.stringify({
            error: { code: 'session_not_found', message: 'no such session' },
          }),
          { status: 404, statusText: 'Not Found' },
        ),
      ),
    )
    let caught: unknown
    try {
      await jsonFetch('/api/foo')
    } catch (e) {
      caught = e
    }
    expect(caught).toBeInstanceOf(ApiClientError)
    expect(caught).toBeInstanceOf(FetchError)
    expect((caught as ApiClientError).code).toBe('session_not_found')
    expect((caught as ApiClientError).status).toBe(404)
    expect((caught as Error).message).toBe('no such session')
  })

  it('falls back to FetchError on non-JSON body', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response('html garbage', { status: 502, statusText: 'Bad Gateway' }),
      ),
    )
    let caught: unknown
    try {
      await jsonFetch('/api/foo')
    } catch (e) {
      caught = e
    }
    expect(caught).toBeInstanceOf(FetchError)
    expect(caught).not.toBeInstanceOf(ApiClientError)
    expect((caught as Error).message).toBe('html garbage')
  })

  it('falls back to FetchError on JSON body that is not an envelope', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ note: 'unstructured' }), {
          status: 400,
          statusText: 'Bad Request',
        }),
      ),
    )
    let caught: unknown
    try {
      await jsonFetch('/api/foo')
    } catch (e) {
      caught = e
    }
    expect(caught).toBeInstanceOf(FetchError)
    expect(caught).not.toBeInstanceOf(ApiClientError)
  })

  it('voidFetch surfaces ApiClientError on typed envelope', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response(
          JSON.stringify({
            error: { code: 'precondition_failure', message: 'not confirmed' },
          }),
          { status: 409, statusText: 'Conflict' },
        ),
      ),
    )
    let caught: unknown
    try {
      await voidFetch('/api/bar')
    } catch (e) {
      caught = e
    }
    expect(caught).toBeInstanceOf(ApiClientError)
    expect((caught as ApiClientError).code).toBe('precondition_failure')
  })
})

describe('jsonFetchRaw', () => {
  it('returns ok+status+data on 2xx', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ hello: 'world' }), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        }),
      ),
    )
    const res = await jsonFetchRaw<{ hello: string }>('/api/foo')
    expect(res.ok).toBe(true)
    expect(res.status).toBe(200)
    expect(res.data).toEqual({ hello: 'world' })
  })

  it('returns ok=false + data=null on non-2xx (no throw)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response('forbidden', { status: 403, statusText: 'Forbidden' }),
      ),
    )
    const res = await jsonFetchRaw('/api/foo')
    expect(res.ok).toBe(false)
    expect(res.status).toBe(403)
    expect(res.data).toBeNull()
  })
})
