// DashboardPane verifies: initial loading state, empty state when no
// completed stages, chip+view selector interaction, and the scatter +
// volcano dispatch paths against committed JSON fixtures.

import { describe, expect, it, vi, afterEach } from 'vitest'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { DashboardPane } from './DashboardTab'

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })
}

function routeFetch(byUrl: Record<string, { status: number; body: unknown }>) {
  const mock = vi.fn().mockImplementation(async (url: string) => {
    const entry = byUrl[url]
    if (!entry) return new Response(null, { status: 404 })
    return jsonResponse(entry.status, entry.body)
  })
  ;(globalThis as unknown as { fetch: typeof fetch }).fetch =
    mock as unknown as typeof fetch
  return mock
}

describe('DashboardPane', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('shows a start-session empty state when sessionId is null', () => {
    render(<DashboardPane sessionId={null} />)
    expect(
      screen.getByTestId('state-dashboard-pane').textContent,
    ).toMatch(/start a session/i)
  })

  it('shows a no-stages empty state when the index is empty', async () => {
    routeFetch({
      '/api/v1/chat/session/s1/dashboard/index': {
        status: 200,
        body: { session_id: 's1', stages: [] },
      },
    })
    render(<DashboardPane sessionId="s1" />)
    await waitFor(() =>
      expect(
        screen.getByTestId('state-dashboard-pane').textContent,
      ).toMatch(/no completed stages/i),
    )
  })

  it('renders a scatter view for embedding_scatter', async () => {
    routeFetch({
      '/api/v1/chat/session/s1/dashboard/index': {
        status: 200,
        body: {
          session_id: 's1',
          stages: [
            {
              stage_id: 'dimensionality_reduction',
              description: 'DR',
              views: [
                {
                  view_id: 'embedding_scatter',
                  data_url: '/api/v1/chat/session/s1/artifacts/runtime/outputs/dimensionality_reduction/view_data/embedding_scatter.json',
                },
              ],
            },
          ],
        },
      },
      '/api/v1/chat/session/s1/artifacts/runtime/outputs/dimensionality_reduction/view_data/embedding_scatter.json':
        {
          status: 200,
          body: {
            stage_id: 'dimensionality_reduction',
            view_id: 'embedding_scatter',
            schema_version: 1,
            data: {
              runs: [
                {
                  id: 'NP',
                  n_points: 3,
                  n_total: 3,
                  x: [0.1, 0.2, 0.3],
                  y: [1.0, 2.0, 3.0],
                },
              ],
            },
          },
        },
    })
    render(<DashboardPane sessionId="s1" />)
    await waitFor(() =>
      expect(screen.getByTestId('dashboard-scatter-view')).toBeInTheDocument(),
    )
    expect(
      screen.getByTestId('dashboard-scatter-view').getAttribute('data-view-id'),
    ).toBe('embedding_scatter')
  })

  it('renders a volcano view for volcano', async () => {
    routeFetch({
      '/api/v1/chat/session/s1/dashboard/index': {
        status: 200,
        body: {
          session_id: 's1',
          stages: [
            {
              stage_id: 'differential_expression',
              description: 'DE',
              views: [
                {
                  view_id: 'volcano',
                  data_url:
                    '/api/v1/chat/session/s1/artifacts/runtime/outputs/differential_expression/view_data/volcano.json',
                },
              ],
            },
          ],
        },
      },
      '/api/v1/chat/session/s1/artifacts/runtime/outputs/differential_expression/view_data/volcano.json':
        {
          status: 200,
          body: {
            stage_id: 'differential_expression',
            view_id: 'volcano',
            schema_version: 1,
            data: {
              comparisons: [
                {
                  id: 'cmp_a',
                  n_total: 4,
                  n_significant: 2,
                  points: {
                    log2fc: [1.5, -1.2, 0.1, 2.0],
                    neg_log10_p: [3.0, 4.0, 0.5, 5.0],
                    significant: [true, true, false, true],
                    labeled: [false, true, false, true],
                    feature: ['g1', 'g2', 'g3', 'g4'],
                  },
                },
              ],
            },
          },
        },
    })
    render(<DashboardPane sessionId="s1" />)
    await waitFor(() =>
      expect(screen.getByTestId('dashboard-volcano-view')).toBeInTheDocument(),
    )
  })

  it('switches between stages when chips are clicked', async () => {
    const index = {
      session_id: 's1',
      stages: [
        {
          stage_id: 'dimensionality_reduction',
          description: 'DR',
          views: [
            {
              view_id: 'embedding_scatter',
              data_url: '/api/v1/chat/session/s1/artifacts/runtime/outputs/dimensionality_reduction/view_data/embedding_scatter.json',
            },
          ],
        },
        {
          stage_id: 'clustering',
          description: 'CL',
          views: [
            {
              view_id: 'umap_by_cluster',
              data_url: '/api/v1/chat/session/s1/artifacts/runtime/outputs/clustering/view_data/umap_by_cluster.json',
            },
          ],
        },
      ],
    }
    routeFetch({
      '/api/v1/chat/session/s1/dashboard/index': { status: 200, body: index },
      '/api/v1/chat/session/s1/artifacts/runtime/outputs/dimensionality_reduction/view_data/embedding_scatter.json':
        {
          status: 200,
          body: {
            data: {
              runs: [
                { id: 'r', n_points: 1, n_total: 1, x: [0], y: [0] },
              ],
            },
          },
        },
      '/api/v1/chat/session/s1/artifacts/runtime/outputs/clustering/view_data/umap_by_cluster.json':
        {
          status: 200,
          body: {
            data: {
              runs: [
                {
                  id: 'r',
                  n_points: 2,
                  n_total: 2,
                  x: [1, 2],
                  y: [3, 4],
                  cluster: ['A', 'B'],
                },
              ],
            },
          },
        },
    })
    render(<DashboardPane sessionId="s1" />)
    await waitFor(() =>
      expect(screen.getByTestId('dashboard-scatter-view')).toBeInTheDocument(),
    )
    expect(
      screen.getByTestId('dashboard-scatter-view').getAttribute('data-stage-id'),
    ).toBe('dimensionality_reduction')
    fireEvent.click(
      screen.getByRole('tab', { name: 'clustering' }),
    )
    await waitFor(() =>
      expect(
        screen.getByTestId('dashboard-scatter-view').getAttribute('data-stage-id'),
      ).toBe('clustering'),
    )
  })

  it('falls back to raw JSON for an unknown view_id', async () => {
    routeFetch({
      '/api/v1/chat/session/s1/dashboard/index': {
        status: 200,
        body: {
          session_id: 's1',
          stages: [
            {
              stage_id: 'some_stage',
              description: 'X',
              views: [
                {
                  view_id: 'weird_view',
                  data_url:
                    '/api/v1/chat/session/s1/artifacts/runtime/outputs/some_stage/view_data/weird_view.json',
                },
              ],
            },
          ],
        },
      },
      '/api/v1/chat/session/s1/artifacts/runtime/outputs/some_stage/view_data/weird_view.json':
        {
          status: 200,
          body: { data: { key: 'value' } },
        },
    })
    render(<DashboardPane sessionId="s1" />)
    await waitFor(() =>
      expect(screen.getByTestId('dashboard-raw-json')).toBeInTheDocument(),
    )
  })
})
