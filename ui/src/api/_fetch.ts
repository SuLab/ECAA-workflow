// Shared fetch wrappers that collapse the
// `if (!res.ok) throw new Error(await res.text()); return res.json()`
// boilerplate. Helpers:
//   * `jsonFetch` for mandatory JSON responses
//   * `jsonFetchOrNull` for the 404-as-null pattern
//   * `voidFetch` for body-less confirm/reject-style endpoints
//   * `jsonFetchRaw` for callers that need the raw status code
//     (e.g. branching on 403 share-token rejection without
//     parsing the throw message).
//
// All accept the full `RequestInit` shape — including `signal`
// — and forward it untouched to the underlying `fetch` call. Callers
// pass an `AbortSignal` for cancellation (session-switch sequencing
// in `useConversation.ts`). `withShareToken` spreads
// the opts object so the signal survives.
//
// The chat surface is also reachable under the
// versioned `/api/v1/chat/...` prefix on the server. The fetch helpers
// rewrite any `/api/chat/...` URL passed by call sites to the
// versioned path before issuing the request, so call sites keep their
// existing string literals and a single switch here moves the whole
// UI to v1.
//
// Typed error envelope (C25): when the server emits a JSON body
// matching the `ApiErrorBody` shape (`{error: {code, message}}`), the
// throw becomes an `ApiClientError` carrying the typed `code`. The
// class extends `FetchError`, so existing `instanceof FetchError`
// branches keep matching; consumers that want the stable code can
// switch to `instanceof ApiClientError && e.code === '...'`.

/**
 * The canonical UI-side chat prefix. Call sites
 * may continue to write `/api/chat/...` literals; [`canonicalize`]
 * rewrites them to `/api/v1/chat/...` at request time. To future-proof:
 * bumping to `/api/v2/chat/` is a one-line change here, no churn
 * across the 130+ call sites.
 */
export const CHAT_API_PREFIX = '/api/v1/chat/'

/**
 * Rewrite legacy `/api/chat/...` paths to the versioned
 * `/api/v1/chat/...` form. Idempotent: paths already on the v1 prefix
 * pass through unchanged. Non-chat URLs (`/api/git/...`,
 * `/artifacts/...`, absolute http(s) URLs) pass through unchanged.
 */
export function canonicalize(url: string): string {
  if (url.startsWith('/api/chat/')) {
    return CHAT_API_PREFIX + url.slice('/api/chat/'.length)
  }
  return url
}

/**
 * When the page was opened with a `share_token` query param, forward
 * it on every API request as an `X-Share-Token` header so the
 * server-side read-only middleware recognises the request. The query
 * param stays in the URL; this header path is the lower-friction
 * alternative to URL-rewriting each API call.
 */
function withShareToken(opts: RequestInit | undefined): RequestInit | undefined {
  if (typeof window === 'undefined') return opts
  const params = new URLSearchParams(window.location.search)
  const tok = params.get('share_token')
  if (!tok) return opts
  const headers = new Headers(opts?.headers ?? {})
  headers.set('X-Share-Token', tok)
  return { ...(opts ?? {}), headers }
}

/**
 * Bearer-token auth shim: when the page was bootstrapped with a
 * `<meta name="swfc-auth-token">` tag, the
 * inline script in `ui/index.html` copies the value to
 * `window.__SWFC_AUTH_TOKEN__`. We attach it on every API request as
 * `Authorization: Bearer <token>` so the server-side
 * `auth_middleware` accepts the call.
 */
function withAuthToken(opts: RequestInit | undefined): RequestInit | undefined {
  if (typeof window === 'undefined') return opts
  const tok = (window as unknown as { __SWFC_AUTH_TOKEN__?: string }).__SWFC_AUTH_TOKEN__
  if (!tok) return opts
  const headers = new Headers(opts?.headers ?? {})
  headers.set('Authorization', `Bearer ${tok}`)
  return { ...(opts ?? {}), headers }
}

/**
 * Compose all per-request header decorators. Order is share-token
 * first (so it inherits the auth header), then auth token. Both are
 * no-ops in non-browser contexts.
 */
function decorate(opts: RequestInit | undefined): RequestInit | undefined {
  return withAuthToken(withShareToken(opts))
}

/**
 * Error subclass thrown by the fetch helpers on non-2xx responses.
 * Carries the HTTP status so call-sites can branch on 404 vs 409 vs
 * other failures without parsing the textual body. Default `message`
 * remains the body text (or a synthesized "status statusText from url"
 * string when the body is empty), so existing `(e as Error).message`
 * consumers keep working.
 */
export class FetchError extends Error {
  status: number
  statusText: string
  url: string
  constructor(status: number, statusText: string, url: string, body: string) {
    super(body || `${status} ${statusText} from ${url}`)
    this.name = 'FetchError'
    this.status = status
    this.statusText = statusText
    this.url = url
  }
}

// ── Typed error envelope (C25) ─────────────────────────────────────────

/**
 * Canonical error envelope the server emits via its typed `ApiError`.
 * Shape: `{ error: { code: "stable_slug", message: "..." } }`.
 * The `code` is a stable, machine-checkable string callers can branch
 * on (`session_not_found`, `precondition_failure`, `forbidden_share_token`,
 * ...); the `message` is the human-readable rendering.
 */
export type ApiErrorBody = {
  error: { code: string; message: string }
}

/**
 * Type predicate. Returns `true` when `x` matches the
 * `ApiErrorBody` shape exactly (object with an `error` object
 * carrying a string `code` and a string `message`). Anything else
 * — raw text, free-form JSON, or partially-shaped bodies — fails
 * the predicate and the caller falls back to the legacy
 * `FetchError(text-or-status-line)` throw.
 */
export function isApiError(x: unknown): x is ApiErrorBody {
  if (typeof x !== 'object' || x === null || !('error' in x)) return false
  const err = (x as { error: unknown }).error
  return (
    typeof err === 'object' &&
    err !== null &&
    'code' in err &&
    typeof (err as { code: unknown }).code === 'string' &&
    'message' in err &&
    typeof (err as { message: unknown }).message === 'string'
  )
}

/**
 * `FetchError` subclass thrown when the response body matched the
 * typed `ApiErrorBody` envelope. Adds a stable `code` field so
 * consumer code can branch on `e instanceof ApiClientError &&
 * e.code === 'forbidden_share_token'` without parsing the message.
 * The base-class `status` + `url` fields still apply.
 */
export class ApiClientError extends FetchError {
  readonly code: string
  constructor(opts: {
    code: string
    message: string
    status: number
    statusText: string
    url: string
  }) {
    super(opts.status, opts.statusText, opts.url, opts.message)
    this.name = 'ApiClientError'
    this.code = opts.code
  }
}

/**
 * Parse the body of a non-2xx response and throw the most specific
 * error we can — `ApiClientError` when the body matches
 * `ApiErrorBody`, otherwise the legacy `FetchError`. Centralized so
 * all four wrappers funnel through one envelope-detection path.
 */
async function throwForNonOk(res: Response, url: string): Promise<never> {
  const body = await res.text()
  if (body.length > 0) {
    try {
      const parsed: unknown = JSON.parse(body)
      if (isApiError(parsed)) {
        throw new ApiClientError({
          code: parsed.error.code,
          message: parsed.error.message,
          status: res.status,
          statusText: res.statusText,
          url,
        })
      }
    } catch (e) {
      if (e instanceof ApiClientError) throw e
      // Otherwise the body wasn't JSON / wasn't ApiErrorBody — fall
      // through to the textual FetchError below.
    }
  }
  throw new FetchError(res.status, res.statusText, url, body)
}

/**
 * Fetch `url`, require a 2xx response, and parse the body as JSON.
 * Throws [`ApiClientError`] (extends `FetchError`) on a typed JSON
 * error body, or [`FetchError`] on any other non-2xx status.
 */
export async function jsonFetch<T>(url: string, opts?: RequestInit): Promise<T> {
  const target = canonicalize(url)
  const res = await fetch(target, decorate(opts))
  if (!res.ok) await throwForNonOk(res, target)
  return res.json()
}

/**
 * Like `jsonFetch` but returns `null` on 404 or 204 instead of throwing.
 * Use for endpoints whose absence is an expected state (e.g., the DAG
 * hasn't been built yet, the metrics table is empty, a decision
 * artifact wasn't written). The 204 case covers servers that
 * explicitly signal "no payload yet" — without it, `res.json()`
 * throws SyntaxError on the empty body.
 */
export async function jsonFetchOrNull<T>(url: string, opts?: RequestInit): Promise<T | null> {
  const target = canonicalize(url)
  const res = await fetch(target, decorate(opts))
  if (res.status === 404 || res.status === 204) return null
  if (!res.ok) await throwForNonOk(res, target)
  return res.json()
}

/**
 * POST / PATCH / DELETE with no response body. Throws on non-2xx.
 * Collapses the `const res = await fetch(...); if (!res.ok) throw...`
 * pattern for confirm / reject / delete-style endpoints.
 */
export async function voidFetch(url: string, opts?: RequestInit): Promise<void> {
  const target = canonicalize(url)
  const res = await fetch(target, decorate(opts))
  if (!res.ok) await throwForNonOk(res, target)
}

/**
 * Status-aware variant of `jsonFetch`. Returns `{ status, ok, data }`
 * instead of throwing on non-2xx, so callers can branch on a specific
 * code (e.g. 403 share-token rejection) without inspecting an
 * exception. The body is parsed as JSON only on 2xx; non-2xx
 * responses surface `data: null` regardless of body shape — call
 * sites that want the typed `ApiErrorBody` for non-2xx should fall
 * back to `jsonFetch` and `catch (e instanceof ApiClientError)`.
 */
export async function jsonFetchRaw<T>(
  url: string,
  opts?: RequestInit,
): Promise<{ status: number; ok: boolean; data: T | null }> {
  const target = canonicalize(url)
  const res = await fetch(target, decorate(opts))
  const data = res.ok ? ((await res.json()) as T) : null
  return { status: res.status, ok: res.ok, data }
}
