// Coverage: placeholder until state.kind === 'emitted'; lists the
// four canonical artifacts once emitted with View links pointing at
// the /artifacts/* endpoint; surfaces the final_report.md card when
// final_reporting wrote one.

import { afterEach, describe, expect, it, vi } from 'vitest'
import { render, waitFor } from '@testing-library/react'

import { DocumentsPane } from './DocumentsTab'
import type { SessionStateSnapshot } from '../../api/chatClient'

function makeState(overrides: Partial<SessionStateSnapshot> = {}): SessionStateSnapshot {
  return {
    session_id: 's1',
    state: { kind: 'intake' } as unknown as SessionStateSnapshot['state'],
    user_confirmed: false,
    last_activity: '2026-04-27T00:00:00Z',
    task_count: 0,
    progress: { completed: 0, ready: 0, blocked: 0, pending: 0 },
    title: null,
    parent_session_id: null,
    blocked_tasks: [], pending_input_hints: [],
    ...overrides,
  }
}

function mockFinalReport(status: number) {
  const mock = vi.fn(async (_url: string) => {
    return new Response(status === 200 ? '# report' : '', { status })
  })
  ;(globalThis as unknown as { fetch: typeof fetch }).fetch =
    mock as unknown as typeof fetch
  return mock
}

afterEach(() => {
  vi.restoreAllMocks()
})

describe('DocumentsPane', () => {
  it('renders placeholder when state is null', () => {
    const { getByText } = render(
      <DocumentsPane state={null} sessionId={null} />,
    )
    expect(getByText(/Documents the package emits/i)).toBeInTheDocument()
  })

  it('renders placeholder when state.kind is not emitted', () => {
    const state = makeState({
      state: { kind: 'intake' } as unknown as SessionStateSnapshot['state'],
      emitted_package_path: '/some/path',
    })
    const { getByText } = render(
      <DocumentsPane state={state} sessionId="s1" />,
    )
    expect(getByText(/Documents the package emits/i)).toBeInTheDocument()
  })

  it('lists the four canonical artifacts as links to /artifacts/ when emitted', () => {
    mockFinalReport(404) // no final report — only artifacts
    const state = makeState({
      state: { kind: 'emitted' } as unknown as SessionStateSnapshot['state'],
      emitted_package_path: '/abs/path/to/package',
    })
    const { getByText, getByLabelText } = render(
      <DocumentsPane state={state} sessionId="s1" />,
    )
    // Every canonical artifact must be a hyperlink, not just a text label.
    for (const name of [
      'WORKFLOW.json',
      'PROMPT.md',
      'CONTEXT.md',
      'ro-crate-metadata.json',
    ]) {
      const a = getByText(name) as HTMLAnchorElement
      expect(a.tagName).toBe('A')
      expect(a.getAttribute('href')).toContain(`/artifacts/${name}`)
      expect(a.getAttribute('target')).toBe('_blank')
    }
    expect(getByLabelText('Package directory')).toHaveTextContent(
      '/abs/path/to/package',
    )
  })

  it('renders a View-inline button on every markdown artifact row', () => {
    mockFinalReport(404)
    const state = makeState({
      state: { kind: 'emitted' } as unknown as SessionStateSnapshot['state'],
      emitted_package_path: '/abs/path/to/package',
    })
    const { getByLabelText, queryByLabelText } = render(
      <DocumentsPane state={state} sessionId="s1" />,
    )
    // Markdown rows get an inline-view affordance.
    expect(getByLabelText('View PROMPT.md inline')).toBeInTheDocument()
    expect(getByLabelText('View CONTEXT.md inline')).toBeInTheDocument()
    // JSON rows do not — the browser renders JSON usably and we don't
    // want a misleading "View inline" on a non-markdown payload.
    expect(queryByLabelText('View WORKFLOW.json inline')).toBeNull()
    expect(queryByLabelText('View ro-crate-metadata.json inline')).toBeNull()
  })

  it('opens the MarkdownViewer when a markdown row View-inline is clicked', async () => {
    mockFinalReport(404)
    const state = makeState({
      state: { kind: 'emitted' } as unknown as SessionStateSnapshot['state'],
      emitted_package_path: '/abs/path/to/package',
    })
    const { getByLabelText, findByTestId } = render(
      <DocumentsPane state={state} sessionId="s1" />,
    )
    const btn = getByLabelText('View PROMPT.md inline') as HTMLButtonElement
    btn.click()
    const dialog = await findByTestId('markdown-viewer-dialog')
    expect(dialog.getAttribute('aria-label')).toContain('PROMPT.md')
  })

  it('shows the Final report card when final_report.md exists', async () => {
    mockFinalReport(200) // 200 = report present
    const state = makeState({
      state: { kind: 'emitted' } as unknown as SessionStateSnapshot['state'],
      emitted_package_path: '/abs/path/to/package',
    })
    const { findByLabelText } = render(
      <DocumentsPane state={state} sessionId="s1" />,
    )
    const card = await findByLabelText('Final report card')
    expect(card.textContent).toMatch(/final report/i)
    // Three affordances: inline viewer, raw markdown link, downloadable.
    expect(card.textContent).toMatch(/view inline/i)
    expect(card.textContent).toMatch(/open raw/i)
    expect(card.textContent).toMatch(/download/i)
  })

  it('shows the Download package button when emitted', () => {
    mockFinalReport(404)
    const state = makeState({
      state: { kind: 'emitted' } as unknown as SessionStateSnapshot['state'],
      emitted_package_path: '/abs/path/to/package',
    })
    const { getByLabelText } = render(
      <DocumentsPane state={state} sessionId="s1" />,
    )
    const link = getByLabelText(
      'Download entire package as gzipped tar archive',
    ) as HTMLAnchorElement
    expect(link.tagName).toBe('A')
    expect(link.getAttribute('href')).toBe(
      '/api/chat/session/s1/package.tar.gz',
    )
    expect(link.hasAttribute('download')).toBe(true)
  })

  it('hides the Final report card when final_reporting has not written one', async () => {
    mockFinalReport(404)
    const state = makeState({
      state: { kind: 'emitted' } as unknown as SessionStateSnapshot['state'],
      emitted_package_path: '/abs/path/to/package',
    })
    const { queryByLabelText } = render(
      <DocumentsPane state={state} sessionId="s1" />,
    )
    // After the probe completes, the card should NOT appear.
    await waitFor(() => {
      expect(queryByLabelText('Final report card')).toBeNull()
    })
  })
})
