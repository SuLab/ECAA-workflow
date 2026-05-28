import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import RemediationSuggestionList from './RemediationSuggestionList'

const SUGGESTIONS_URL_RE = /\/remediation-suggestions$/
const APPLY_URL_RE = /\/apply-remediation$/

const ENVELOPE_OOM = {
  task_id: 'alignment',
  stage_id: 'alignment',
  library: 'STAR',
  error_class: 'OOM',
  message: 'killed (SIGKILL)',
  stderr_tail: ['STAR: out of memory'],
  stdout_tail: [],
  exit_code: 137,
  signal: 'SIGKILL',
  peak_memory_mb: 31000,
  wallclock_secs: 1500,
  input_summary: {},
  executor: 'local',
  executor_context: { host: 'box01' },
  captured_at: '2026-05-04T00:00:00Z',
  attempt: 1,
  schema_version: 1,
}

function fetchMock(handlers: Array<{ match: RegExp; resolve: () => Response | Promise<Response> }>) {
  return vi.fn(async (url: RequestInfo | URL) => {
    const u = typeof url === 'string' ? url : (url as URL).toString()
    for (const h of handlers) {
      if (h.match.test(u)) return await h.resolve()
    }
    throw new Error(`unexpected fetch: ${u}`)
  })
}

beforeEach(() => {
  // jsdom default: no fetch
  (globalThis as unknown as { fetch: unknown }).fetch = vi.fn()
})

afterEach(() => {
  vi.restoreAllMocks()
})

describe('RemediationSuggestionList', () => {
  it('renders ranked suggestions with confidence + evidence', async () => {
    const body = {
      envelope: ENVELOPE_OOM,
      attempts_consumed: 0,
      regenerated: true,
      suggestions: [
        {
          id: 'rs-1',
          kind: { kind: 'bump_resources', target: { memory_gb: 64 }, prior: { memory_gb: 32 } },
          rationale: 'STAR ran out of memory at 32 GiB; bump to 64.',
          confidence: 'high',
          evidence: ['error_class', 'signal'],
          tool_binding: 'rerun_task',
          estimated_cost_delta_usd: 0.45,
        },
      ],
    }
    ;(globalThis as unknown as { fetch: ReturnType<typeof fetchMock> }).fetch = fetchMock([
      {
        match: SUGGESTIONS_URL_RE,
        resolve: () => new Response(JSON.stringify(body), { status: 200, headers: { 'content-type': 'application/json' } }),
      },
    ])

    render(<RemediationSuggestionList sessionId="s1" taskId="alignment" />)

    await waitFor(() => screen.getByText('Bump resources'))
    expect(screen.getByText(/STAR ran out of memory/)).toBeInTheDocument()
    expect(screen.getByText('class:').parentElement?.textContent).toContain('OOM')
    expect(screen.getByText('error_class')).toBeInTheDocument()
    expect(screen.getByText('signal')).toBeInTheDocument()
    expect(screen.getByText('high')).toBeInTheDocument()
    expect(screen.getByText('+$0.45/run')).toBeInTheDocument()
    expect(screen.getByText('Apply & rerun')).toBeInTheDocument()
  })

  it('clicking Apply hits POST /apply-remediation and notifies parent', async () => {
    const onApplied = vi.fn()
    const suggestionsBody = {
      envelope: ENVELOPE_OOM,
      attempts_consumed: 0,
      regenerated: true,
      suggestions: [
        {
          id: 'rs-1',
          kind: { kind: 'retry_as_is', reason: 'transient' },
          rationale: 'Single S3 503 — retry once.',
          confidence: 'medium',
          evidence: ['error_class'],
          tool_binding: 'rerun_task',
        },
      ],
    }
    const applyBody = {
      suggestion_id: 'rs-1',
      tool_binding: 'rerun_task',
      outcome: 'applied',
      message: 'Applied retry-as-is and reran task alignment.',
      overrides_path: '/pkg/runtime/inputs/alignment/overrides.json',
    }
    let applyCalled = false
    ;(globalThis as unknown as { fetch: ReturnType<typeof fetchMock> }).fetch = fetchMock([
      {
        match: APPLY_URL_RE,
        resolve: () => {
          applyCalled = true
          return new Response(JSON.stringify(applyBody), { status: 200, headers: { 'content-type': 'application/json' } })
        },
      },
      {
        match: SUGGESTIONS_URL_RE,
        resolve: () =>
          new Response(JSON.stringify(suggestionsBody), { status: 200, headers: { 'content-type': 'application/json' } }),
      },
    ])

    render(<RemediationSuggestionList sessionId="s1" taskId="alignment" onApplied={onApplied} />)
    await waitFor(() => screen.getByText('Retry as-is'))

    fireEvent.click(screen.getByText('Apply & rerun'))
    await waitFor(() => expect(applyCalled).toBe(true))
    await waitFor(() => expect(onApplied).toHaveBeenCalledWith(applyBody))
    expect(screen.getByText(/Applied retry-as-is/)).toBeInTheDocument()
  })

  it('renders empty state when proposer returns no suggestions', async () => {
    const body = { envelope: ENVELOPE_OOM, attempts_consumed: 0, regenerated: true, suggestions: [] }
    ;(globalThis as unknown as { fetch: ReturnType<typeof fetchMock> }).fetch = fetchMock([
      {
        match: SUGGESTIONS_URL_RE,
        resolve: () => new Response(JSON.stringify(body), { status: 200, headers: { 'content-type': 'application/json' } }),
      },
    ])
    render(<RemediationSuggestionList sessionId="s1" taskId="alignment" />)
    await waitFor(() => screen.getByText(/Manual review required/))
  })

  it('renders error message when envelope is missing (404)', async () => {
    (globalThis as unknown as { fetch: ReturnType<typeof fetchMock> }).fetch = fetchMock([
      {
        match: SUGGESTIONS_URL_RE,
        resolve: () => new Response('not found', { status: 404 }),
      },
    ])
    render(<RemediationSuggestionList sessionId="s1" taskId="alignment" />)
    await waitFor(() => screen.getByText(/No structured error envelope/))
  })

  it('renders KindDetail for switch_method with from→to', async () => {
    const body = {
      envelope: ENVELOPE_OOM,
      attempts_consumed: 0,
      regenerated: true,
      suggestions: [
        {
          id: 'rs-1',
          kind: { kind: 'switch_method', stage_id: 'alignment', from: 'STAR', to: 'HISAT2', switch_kind: 'library' },
          rationale: 'STAR memory profile too tight; HISAT2 uses ~half the RAM.',
          confidence: 'medium',
          evidence: ['library', 'peak_memory_mb'],
          tool_binding: 'amend_stage_method',
        },
      ],
    }
    ;(globalThis as unknown as { fetch: ReturnType<typeof fetchMock> }).fetch = fetchMock([
      {
        match: SUGGESTIONS_URL_RE,
        resolve: () => new Response(JSON.stringify(body), { status: 200, headers: { 'content-type': 'application/json' } }),
      },
    ])
    render(<RemediationSuggestionList sessionId="s1" taskId="alignment" />)
    await waitFor(() => screen.getByText('Switch library'))
    expect(screen.getByText('STAR → HISAT2')).toBeInTheDocument()
    expect(screen.getByText('Record method swap')).toBeInTheDocument()
  })
})
