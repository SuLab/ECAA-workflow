// SessionTitleBar rendering + button behavior.
//
// Each test resets the module-level config cache so a fake fetch
// response in one test doesn't leak into the next.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'

import SessionTitleBar, {
  __resetSessionTitleBarCache,
} from './SessionTitleBar'

type FetchResponse = {
  ok: boolean
  status: number
  json?: () => Promise<unknown>
  text?: () => Promise<string>
}
type FetchHandler = (input: string, init?: RequestInit) => Promise<FetchResponse>

function installFetch(handler: FetchHandler): void {
  // The component calls the real `fetch`; stub it with a matcher-based
  // handler. Tests assert against the captured URLs as needed.
  (globalThis as unknown as { fetch: FetchHandler }).fetch = handler
}

function jsonResponse(body: unknown): FetchResponse {
  return {
    ok: true,
    status: 200,
    json: async () => body,
    text: async () => JSON.stringify(body),
  }
}

beforeEach(() => {
  __resetSessionTitleBarCache()
})

afterEach(() => {
  vi.restoreAllMocks()
})

describe('SessionTitleBar', () => {
  it('renders nothing when no session is active', () => {
    installFetch(async () => jsonResponse({ auto_title_enabled: true, auto_title_min_turns: 3 }))
    const { container } = render(
      <SessionTitleBar
        sessionId={null}
        title={null}
        turnCount={0}
        onTitled={vi.fn()}
      />,
    )
    // The component returns null on no session; the render wrapper div is empty.
    expect(container.firstChild).toBeNull()
  })

  it('renders the title text when a title is present (no button)', async () => {
    // Even though the feature flag is on and turn count meets the
    // threshold, a session that already has a title should just show
    // the text — the button is gone.
    installFetch(async () => jsonResponse({ auto_title_enabled: true, auto_title_min_turns: 3 }))
    render(
      <SessionTitleBar
        sessionId="abc123"
        title="Bulk RNA-seq DE liver"
        turnCount={10}
        onTitled={vi.fn()}
      />,
    )
    expect(screen.getByText('Bulk RNA-seq DE liver')).toBeInTheDocument()
    expect(screen.queryByRole('button')).toBeNull()
  })

  it('hides the button when the feature flag is off', async () => {
    installFetch(async () => jsonResponse({ auto_title_enabled: false, auto_title_min_turns: 3 }))
    const { container } = render(
      <SessionTitleBar
        sessionId="abc123"
        title={null}
        turnCount={5}
        onTitled={vi.fn()}
      />,
    )
    // Wait for the config fetch to resolve + render settle.
    await waitFor(() => {
      expect(container.querySelector('[data-session-title="button"]')).toBeNull()
    })
  })

  it('hides the button when turn count is below auto_title_min_turns', async () => {
    installFetch(async () => jsonResponse({ auto_title_enabled: true, auto_title_min_turns: 3 }))
    const { container } = render(
      <SessionTitleBar
        sessionId="abc123"
        title={null}
        turnCount={2}
        onTitled={vi.fn()}
      />,
    )
    await waitFor(() => {
      // Config has resolved; button gate still blocks on turn count.
      expect(container.querySelector('[data-session-title="button"]')).toBeNull()
    })
  })

  it('shows the button when flag is on and turn count meets threshold', async () => {
    installFetch(async () => jsonResponse({ auto_title_enabled: true, auto_title_min_turns: 3 }))
    render(
      <SessionTitleBar
        sessionId="abc123"
        title={null}
        turnCount={3}
        onTitled={vi.fn()}
      />,
    )
    const button = await screen.findByRole('button', { name: /auto-name/i })
    expect(button).toBeInTheDocument()
    expect(button).not.toBeDisabled()
  })

  it('calls the auto-title endpoint and fires onTitled on success', async () => {
    const captured: string[] = []
    installFetch(async (url) => {
      captured.push(url)
      if (url === '/api/v1/chat/config') {
        return jsonResponse({ auto_title_enabled: true, auto_title_min_turns: 3 })
      }
      if (url.endsWith('/auto-title')) {
        return jsonResponse({ title: 'Generated title', from_cache: false })
      }
      return { ok: false, status: 404, text: async () => 'not found' }
    })
    const onTitled = vi.fn()
    render(
      <SessionTitleBar
        sessionId="abc123"
        title={null}
        turnCount={3}
        onTitled={onTitled}
      />,
    )
    const button = await screen.findByRole('button', { name: /auto-name/i })
    fireEvent.click(button)
    await waitFor(() => {
      expect(onTitled).toHaveBeenCalledWith('Generated title')
    })
    expect(captured).toContain('/api/v1/chat/session/abc123/auto-title')
  })

  it('surfaces an error badge when the endpoint fails', async () => {
    installFetch(async (url) => {
      if (url === '/api/v1/chat/config') {
        return jsonResponse({ auto_title_enabled: true, auto_title_min_turns: 3 })
      }
      return {
        ok: false,
        status: 500,
        json: async () => ({}),
        text: async () => 'boom',
      }
    })
    const onTitled = vi.fn()
    render(
      <SessionTitleBar
        sessionId="abc123"
        title={null}
        turnCount={3}
        onTitled={onTitled}
      />,
    )
    const button = await screen.findByRole('button', { name: /auto-name/i })
    fireEvent.click(button)
    await waitFor(() => {
      expect(screen.getByRole('alert')).toBeInTheDocument()
    })
    // onTitled should NOT fire when the server rejected the request.
    expect(onTitled).not.toHaveBeenCalled()
  })

  it('degrades gracefully when /api/v1/chat/config itself fails (old server)', async () => {
    installFetch(async (url) => {
      if (url === '/api/v1/chat/config') {
        return { ok: false, status: 404, text: async () => 'not found' }
      }
      return { ok: false, status: 404, text: async () => 'not found' }
    })
    const { container } = render(
      <SessionTitleBar
        sessionId="abc123"
        title={null}
        turnCount={5}
        onTitled={vi.fn()}
      />,
    )
    await waitFor(() => {
      // No button renders when config fetch fails — treated as flag-off.
      expect(container.querySelector('[data-session-title="button"]')).toBeNull()
    })
  })
})
