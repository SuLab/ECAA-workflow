import { beforeEach, expect, it, vi } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import App from './App'
import { ThemeProvider } from './hooks/useTheme'
import { importPackage } from './api/chatClient'

const minimalCaps = {
  tier_label: 'minimal_audit' as const,
  explore: true,
  reverify: true,
  replay_tier1: true,
  replay_tier2: false,
  tabs: {},
}

vi.mock('./api/chatClient', async (orig) => {
  const actual = await orig<typeof import('./api/chatClient')>()
  return {
    ...actual,
    createChatSession: vi.fn(async () => ({
      session_id: 's0',
      greeting: {
        turn_id: 't',
        role: 'assistant',
        content: 'hi',
        intent: null,
        tool_calls: [],
        quick_replies: [],
        confirmation_card: null,
        timestamp: '2026-07-08T00:00:00Z',
      },
    })),
    getChatState: vi.fn(async (id: string) => ({
      session_id: id,
      state: { kind: 'greeting' },
      user_confirmed: false,
      last_activity: '',
      task_count: 0,
      progress: { completed: 0, ready: 0, blocked: 0, pending: 0 },
      title: null,
      parent_session_id: null,
      blocked_tasks: [],
      pending_input_hints: [],
    })),
    getChatDag: vi.fn(async () => null),
    getChatTranscript: vi.fn(async () => []),
    getHarnessEventsBacklog: vi.fn(async () => ({ events: [] })),
    getComposeOutcome: vi.fn(async () => null),
    getPilot: vi.fn(async () => null),
    getChatMetrics: vi.fn(async () => null),
    getExecution: vi.fn(async () => null),
    importPackage: vi.fn(async () => ({
      session_id: 'imp1',
      imported: true,
      capabilities: minimalCaps,
    })),
    getCapabilities: vi.fn(async () => ({
      imported: true,
      capabilities: minimalCaps,
    })),
  }
})

beforeEach(() => {
  // jsdom has no EventSource; the SSE hook opens one once a session is set.
  class EventSourceStub {
    onopen: ((this: EventSource, ev: Event) => unknown) | null = null
    onerror: ((this: EventSource, ev: Event) => unknown) | null = null
    onmessage: ((this: EventSource, ev: MessageEvent) => unknown) | null = null
    url: string
    constructor(url: string) {
      this.url = url
    }
    addEventListener(): void {}
    removeEventListener(): void {}
    close(): void {}
  }
  vi.stubGlobal('EventSource', EventSourceStub)
  // Any non-mocked chatClient call funnels through fetch; reject so the
  // effects' try/catch swallow it deterministically (no real network).
  vi.stubGlobal(
    'fetch',
    vi.fn(() => Promise.reject(new Error('no network in test'))),
  )
})

it('Open a package uploads and switches to the imported read-only session', async () => {
  render(
    <ThemeProvider>
      <App />
    </ThemeProvider>,
  )

  const input = await screen.findByTestId('open-package-input')
  const file = new File([new Uint8Array([0x50, 0x4b, 0x03, 0x04])], 'pkg.zip', {
    type: 'application/zip',
  })
  await userEvent.upload(input, file)

  await waitFor(() => expect(importPackage).toHaveBeenCalled())
  await waitFor(() =>
    expect(screen.getByTestId('imported-badge')).toBeInTheDocument(),
  )
  expect(screen.getByTestId('imported-badge').textContent).toMatch(/read-only/i)
})

it('Open a package surfaces an invalid archive error', async () => {
  vi.mocked(importPackage).mockRejectedValueOnce(
    new Error('unsupported archive format'),
  )

  render(
    <ThemeProvider>
      <App />
    </ThemeProvider>,
  )

  const input = await screen.findByTestId('open-package-input')
  const file = new File(['not an archive'], 'bad.zip', {
    type: 'application/zip',
  })
  await userEvent.upload(input, file)

  expect(await screen.findByRole('alert')).toHaveTextContent(
    'Could not open package: unsupported archive format',
  )
  expect(screen.getByTestId('session-id-prefix')).toHaveTextContent('s0')
})
