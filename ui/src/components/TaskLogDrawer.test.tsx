// The drawer polls runtime/outputs/<task_id>/progress.log via the
// /api/chat/session/:id/artifacts/*path endpoint. These tests mock
// fetch and verify rendering for three states (empty / loaded /
// errored), plus the close button and autoscroll.

import { describe, expect, it, vi, afterEach } from 'vitest'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import TaskLogDrawer from './TaskLogDrawer'

function mockFetch(responses: Array<Response | Promise<Response>>) {
  const mock = vi.fn()
  for (const r of responses) mock.mockResolvedValueOnce(r)
  ;(globalThis as unknown as { fetch: typeof fetch }).fetch = mock as unknown as typeof fetch
  return mock
}

function textResponse(status: number, body: string): Response {
  return new Response(body, {
    status,
    headers: { 'Content-Type': 'text/plain' },
  })
}

describe('TaskLogDrawer', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('renders nothing when sessionId is null', () => {
    const { container } = render(
      <TaskLogDrawer sessionId={null} taskId="t1" onClose={() => {}} />,
    )
    expect(container.firstChild).toBeNull()
  })

  it('renders nothing when taskId is null', () => {
    const { container } = render(
      <TaskLogDrawer sessionId="s1" taskId={null} onClose={() => {}} />,
    )
    expect(container.firstChild).toBeNull()
  })

  it('shows the empty state when the log file is 404', async () => {
    mockFetch([textResponse(404, 'not found')])
    render(
      <TaskLogDrawer sessionId="s1" taskId="t1" onClose={() => {}} />,
    )
    await waitFor(() =>
      expect(
        screen.getByTestId('task-log-body').textContent,
      ).toMatch(/no progress log yet/i),
    )
  })

  it('renders the log body when the endpoint returns text', async () => {
    const body = [
      '[2026-04-17T17:30:00] fetching GSE156063 supplementary files',
      '[2026-04-17T17:30:02] downloaded 5.1 MB',
      '[2026-04-17T17:30:05] matched 234 samples to metadata',
    ].join('\n')
    mockFetch([textResponse(200, body)])
    render(
      <TaskLogDrawer
        sessionId="s1"
        taskId="data_acquisition"
        onClose={() => {}}
      />,
    )
    await waitFor(() =>
      expect(
        screen.getByTestId('task-log-body').textContent,
      ).toContain('downloaded 5.1 MB'),
    )
  })

  it('calls onClose when the close button is clicked', async () => {
    mockFetch([textResponse(200, 'hi')])
    const onClose = vi.fn()
    render(
      <TaskLogDrawer sessionId="s1" taskId="t1" onClose={onClose} />,
    )
    await waitFor(() =>
      expect(screen.getByLabelText('Close log drawer')).toBeInTheDocument(),
    )
    fireEvent.click(screen.getByLabelText('Close log drawer'))
    expect(onClose).toHaveBeenCalledTimes(1)
  })

  it('shows an error indicator when fetch throws', async () => {
    (globalThis as unknown as { fetch: typeof fetch }).fetch = vi
      .fn()
      .mockRejectedValueOnce(new Error('network down')) as unknown as typeof fetch
    render(
      <TaskLogDrawer sessionId="s1" taskId="t1" onClose={() => {}} />,
    )
    await waitFor(() =>
      expect(
        screen.getByTestId('task-log-body').textContent,
      ).toContain('network down'),
    )
  })

  it('surfaces the task_id in the header', async () => {
    mockFetch([textResponse(200, 'ok')])
    render(
      <TaskLogDrawer
        sessionId="s1"
        taskId="discover_normalization"
        onClose={() => {}}
      />,
    )
    await waitFor(() =>
      expect(screen.getByText('discover_normalization')).toBeInTheDocument(),
    )
  })

  it('exposes the autoscroll toggle', async () => {
    mockFetch([textResponse(200, 'ok')])
    render(
      <TaskLogDrawer sessionId="s1" taskId="t1" onClose={() => {}} />,
    )
    await waitFor(() =>
      expect(screen.getByLabelText('Autoscroll')).toBeInTheDocument(),
    )
    expect(screen.getByLabelText('Autoscroll')).toBeChecked()
    fireEvent.click(screen.getByLabelText('Autoscroll'))
    expect(screen.getByLabelText('Autoscroll')).not.toBeChecked()
  })

  // ---- Figures tab ----------------------------------------------------

  function jsonResponse(status: number, body: unknown): Response {
    return new Response(JSON.stringify(body), {
      status,
      headers: { 'Content-Type': 'application/json' },
    })
  }

  it('switching to Figures tab with no manifest shows the awaiting-figures copy', async () => {
    // First call (log poll) + first call (figures manifest fetch)
    mockFetch([textResponse(200, 'log body'), jsonResponse(404, {})])
    render(
      <TaskLogDrawer sessionId="s1" taskId="clustering" onClose={() => {}} />,
    )
    await waitFor(() =>
      expect(screen.getByLabelText('Close log drawer')).toBeInTheDocument(),
    )
    fireEvent.click(screen.getByRole('tab', { name: /figures/i }))
    await waitFor(() =>
      expect(screen.getByTestId('task-figures-panel').textContent).toMatch(
        /No figures yet/i,
      ),
    )
  })

  it('switching to Figures tab with a populated manifest renders thumbnails', async () => {
    const manifest = {
      stage_id: 'clustering',
      written: {
        umap_clusters:
          '/pkg/runtime/outputs/clustering/figures/umap_clusters.png',
        cluster_size_bar:
          '/pkg/runtime/outputs/clustering/figures/cluster_size_bar.png',
      },
      skipped: {},
      errors: {},
    }
    mockFetch([textResponse(200, 'log body'), jsonResponse(200, manifest)])
    render(
      <TaskLogDrawer sessionId="s1" taskId="clustering" onClose={() => {}} />,
    )
    await waitFor(() =>
      expect(screen.getByLabelText('Close log drawer')).toBeInTheDocument(),
    )
    fireEvent.click(screen.getByRole('tab', { name: /figures/i }))
    await waitFor(() =>
      expect(
        screen.getByLabelText('Open figure umap_clusters'),
      ).toBeInTheDocument(),
    )
    expect(
      screen.getByLabelText('Open figure cluster_size_bar'),
    ).toBeInTheDocument()
  })

  it('clicking a thumbnail opens the lightbox and Escape closes it', async () => {
    const manifest = {
      stage_id: 'clustering',
      written: {
        umap_clusters:
          '/pkg/runtime/outputs/clustering/figures/umap_clusters.png',
      },
      skipped: {},
      errors: {},
    }
    mockFetch([textResponse(200, 'log'), jsonResponse(200, manifest)])
    render(
      <TaskLogDrawer sessionId="s1" taskId="clustering" onClose={() => {}} />,
    )
    await waitFor(() =>
      expect(screen.getByLabelText('Close log drawer')).toBeInTheDocument(),
    )
    fireEvent.click(screen.getByRole('tab', { name: /figures/i }))
    await waitFor(() =>
      expect(
        screen.getByLabelText('Open figure umap_clusters'),
      ).toBeInTheDocument(),
    )
    fireEvent.click(screen.getByLabelText('Open figure umap_clusters'))
    expect(screen.getByTestId('figure-lightbox')).toBeInTheDocument()
    fireEvent.keyDown(window, { key: 'Escape' })
    await waitFor(() =>
      expect(screen.queryByTestId('figure-lightbox')).not.toBeInTheDocument(),
    )
  })
})
