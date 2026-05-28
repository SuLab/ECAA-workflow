// Top-level Figures tab. Mocks fetch() to return per-stage
// figures/manifest.json payloads and verifies the cross-stage gallery
// behavior: aggregates tasks whose DAG spec declares required_figures,
// polls each manifest, renders sections, and shows appropriate empty
// states for no-session / no-declarations / no-figures-yet.
//
// The artifact-URL fetch now goes through `jsonFetch`, which rewrites
// `/api/chat/...` to `/api/v1/chat/...` via the canonical chat-API
// prefix. The URL-keyed mock therefore also normalises legacy paths so
// existing fixture URLs keep matching after the migration.

import { describe, expect, it, vi, afterEach } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import { FiguresPane } from './FiguresTab'
import type { DAG } from '../../types'

function mockFetch(byUrl: Record<string, { status: number; body: unknown }>) {
  // Accept either the legacy `/api/chat/...` form or the post-
  // canonicalize `/api/v1/chat/...` form in mock keys — keeps the
  // existing fixtures readable without forcing a churn.
  const normalised: Record<string, { status: number; body: unknown }> = {}
  for (const [k, v] of Object.entries(byUrl)) {
    normalised[k] = v
    if (k.startsWith('/api/chat/')) {
      normalised['/api/v1/chat/' + k.slice('/api/chat/'.length)] = v
    }
  }
  const mock = vi.fn().mockImplementation(async (url: string) => {
    const entry = normalised[url]
    if (!entry) return new Response(null, { status: 404 })
    return new Response(JSON.stringify(entry.body), {
      status: entry.status,
      headers: { 'Content-Type': 'application/json' },
    })
  })
  ;(globalThis as unknown as { fetch: typeof fetch }).fetch =
    mock as unknown as typeof fetch
  return mock
}

type TaskStatusForTest =
  | 'pending'
  | 'ready'
  | 'running'
  | 'completed'
  | 'blocked'
  | 'failed'

function mkDag(
  tasks: Record<
    string,
    { required_figures?: string[]; status?: TaskStatusForTest }
  >,
): DAG {
  const stateFor = (status: TaskStatusForTest): Record<string, unknown> => {
    switch (status) {
      case 'pending':
        return { status: 'pending' }
      case 'ready':
        return { status: 'ready' }
      case 'running':
        return { status: 'running', started_at: '2026-01-01T00:00:00Z' }
      case 'completed':
        return { status: 'completed', result: {} }
      case 'blocked':
        return { status: 'blocked', record: {} }
      case 'failed':
        return { status: 'failed', reason: 'test' }
    }
  }
  const out: Record<string, unknown> = {}
  for (const [tid, props] of Object.entries(tasks)) {
    out[tid] = {
      kind: 'computation',
      state: stateFor(props.status ?? 'completed'),
      depends_on: [],
      assignee: 'agent',
      description: tid,
      spec: props.required_figures
        ? { stage_class: tid, required_figures: props.required_figures }
        : null,
      resolution: null,
      result_ref: null,
      resource_class: null,
      requires_sme_review: null,
    }
  }
  return { workflow_id: 'w', tasks: out } as unknown as DAG
}

describe('FiguresPane', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('shows start-a-session empty state when sessionId is null', () => {
    render(<FiguresPane sessionId={null} dag={mkDag({})} />)
    expect(
      screen.getByTestId('state-figures-pane').textContent,
    ).toMatch(/start a session/i)
  })

  it('shows no-tasks empty state when DAG is empty', () => {
    render(<FiguresPane sessionId="s1" dag={mkDag({})} />)
    expect(
      screen.getByTestId('state-figures-pane').textContent,
    ).toMatch(/no workflow tasks yet/i)
  })

  it('shows no-figures-produced when probes return nothing', async () => {
    // Behavior change: instead of filtering on declarative
    // `task.spec.required_figures`, the tab now probes every task's
    // `figures/manifest.json` directly. When every probe 404s, the
    // empty state reports "No figures produced yet".
    mockFetch({}) // every URL 404s
    const dag = mkDag({
      quality_control: {},
      clustering: {},
    })
    render(<FiguresPane sessionId="s1" dag={dag} />)
    await waitFor(() =>
      expect(
        screen.getByTestId('state-figures-pane').textContent,
      ).toMatch(/no figures produced yet/i),
    )
  })

  it('surfaces figures from tasks that did not declare required_figures', async () => {
    // Regression: the OLD UI filtered on task.spec.required_figures
    // before fetching manifests. The v4 composer doesn't populate
    // required_figures for every plot-producing stage (e.g.
    // differential_expression), so figures rendered on disk would
    // never appear in the UI. The new behavior probes every task in
    // the DAG.
    const dag = mkDag({
      differential_expression: {}, // no required_figures declared
    })
    mockFetch({
      '/api/chat/session/s1/artifacts/runtime/outputs/differential_expression/figures/manifest.json':
        {
          status: 200,
          body: {
            stage_id: 'differential_expression',
            written: {
              volcano:
                '/pkg/runtime/outputs/differential_expression/figures/volcano.png',
            },
            skipped: {},
            errors: {},
          },
        },
    })
    render(<FiguresPane sessionId="s1" dag={dag} />)
    await waitFor(() =>
      expect(
        screen.getByLabelText('Figures for differential_expression'),
      ).toBeInTheDocument(),
    )
  })

  it('renders a section per stage when manifests arrive', async () => {
    const dag = mkDag({
      quality_control: { required_figures: ['per_sample_metric_violin'] },
      clustering: { required_figures: ['umap_clusters'] },
    })
    mockFetch({
      '/api/chat/session/s1/artifacts/runtime/outputs/quality_control/figures/manifest.json':
        {
          status: 200,
          body: {
            stage_id: 'quality_control',
            written: {
              per_sample_metric_violin:
                '/pkg/runtime/outputs/quality_control/figures/per_sample_metric_violin.png',
            },
            skipped: {},
            errors: {},
          },
        },
      '/api/chat/session/s1/artifacts/runtime/outputs/clustering/figures/manifest.json':
        {
          status: 200,
          body: {
            stage_id: 'clustering',
            written: {
              umap_clusters:
                '/pkg/runtime/outputs/clustering/figures/umap_clusters.png',
            },
            skipped: {},
            errors: {},
          },
        },
    })
    render(<FiguresPane sessionId="s1" dag={dag} />)
    await waitFor(() => {
      expect(
        screen.getByLabelText('Figures for quality_control'),
      ).toBeInTheDocument()
      expect(screen.getByLabelText('Figures for clustering')).toBeInTheDocument()
    })
    const ids = Array.from(
      document.querySelectorAll('[data-figure-id]'),
    ).map((e) => e.getAttribute('data-figure-id'))
    expect(ids).toContain('per_sample_metric_violin')
    expect(ids).toContain('umap_clusters')
  })

  it('only polls manifests for tasks whose state can have figures', async () => {
    // Regression: the tab used to probe `figures/manifest.json` for
    // every task in the DAG regardless of state. On a 23-task session
    // that surfaced 100+ console 404s per polling cycle (one per
    // pending/discover_* task). The filter keeps probes scoped to
    // states that can actually have written figures.
    const dag = mkDag({
      done_one: { status: 'completed' },
      done_two: { status: 'completed' },
      pending_one: { status: 'pending' },
      pending_two: { status: 'pending' },
      discover_modality: { status: 'ready' },
    })
    const fetchMock = mockFetch({}) // every URL 404s; we only assert call shape
    render(<FiguresPane sessionId="s1" dag={dag} />)
    await waitFor(() => {
      expect(fetchMock).toHaveBeenCalled()
    })
    const probedUrls = fetchMock.mock.calls.map((c) => String(c[0]))
    const manifestProbes = probedUrls.filter((u) =>
      u.includes('/figures/manifest.json'),
    )
    expect(manifestProbes).toHaveLength(2)
    expect(
      manifestProbes.some((u) => u.includes('/outputs/done_one/')),
    ).toBe(true)
    expect(
      manifestProbes.some((u) => u.includes('/outputs/done_two/')),
    ).toBe(true)
    expect(
      manifestProbes.some((u) => u.includes('/outputs/pending_one/')),
    ).toBe(false)
    expect(
      manifestProbes.some((u) => u.includes('/outputs/pending_two/')),
    ).toBe(false)
    expect(
      manifestProbes.some((u) => u.includes('/outputs/discover_modality/')),
    ).toBe(false)
  })

  it('handles schema-4 manifest (figures as array of {figure_id, png, pdf})', async () => {
    // Regression: time_series_decompose's R-script renderer emits
    // `figures: [{figure_id, png, pdf, status}]` — an array. The
    // normalizer used to coerce arrays to dicts via Object.entries,
    // turning the indices 0,1,2,3 into figure_ids. The figures became
    // invisible/mislabeled. Now the array case is detected explicitly
    // and projected to a dict keyed by the inner figure_id field.
    const dag = mkDag({
      time_series_decompose: {},
    })
    mockFetch({
      '/api/chat/session/s1/artifacts/runtime/outputs/time_series_decompose/figures/manifest.json':
        {
          status: 200,
          body: {
            stage_id: 'time_series_decompose',
            figures: [
              {
                figure_id: 'decomposition_panel',
                png: 'decomposition_panel.png',
                pdf: 'decomposition_panel.pdf',
                status: 'written',
              },
              {
                figure_id: 'acf_pacf_panel',
                png: 'acf_pacf_panel.png',
                pdf: 'acf_pacf_panel.pdf',
                status: 'written',
              },
            ],
          },
        },
    })
    render(<FiguresPane sessionId="s1" dag={dag} />)
    await waitFor(() =>
      expect(
        screen.getByLabelText('Figures for time_series_decompose'),
      ).toBeInTheDocument(),
    )
    // Both figure ids must be projected from the array (NOT mislabeled
    // as numeric indices 0/1).
    expect(screen.getByText('decomposition_panel')).toBeInTheDocument()
    expect(screen.getByText('acf_pacf_panel')).toBeInTheDocument()
  })

  it('skips sections whose manifest has no written entries', async () => {
    const dag = mkDag({
      quality_control: { required_figures: ['per_sample_metric_violin'] },
    })
    mockFetch({
      '/api/chat/session/s1/artifacts/runtime/outputs/quality_control/figures/manifest.json':
        {
          status: 200,
          body: {
            stage_id: 'quality_control',
            written: {},
            skipped: { per_sample_metric_violin: 'input missing' },
            errors: {},
          },
        },
    })
    render(<FiguresPane sessionId="s1" dag={dag} />)
    await waitFor(() =>
      expect(
        screen.getByTestId('state-figures-pane').textContent,
      ).toMatch(/no figures produced yet/i),
    )
  })
})
