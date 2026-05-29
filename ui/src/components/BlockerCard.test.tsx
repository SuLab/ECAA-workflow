import { describe, expect, it, vi, afterEach } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import BlockerCard from './BlockerCard'

function mockFetch(responses: Array<Response | Promise<Response>>) {
  const mock = vi.fn()
  for (const r of responses) mock.mockResolvedValueOnce(r)
  ;(globalThis as unknown as { fetch: typeof fetch }).fetch = mock as unknown as typeof fetch
  return mock
}

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })
}

describe('BlockerCard', () => {
  it('renders the reason and the recovery hint', () => {
    render(
      <BlockerCard
        reason="anthropic API unreachable: connection refused after 3 retries"
        recoveryHint="Wait for the underlying service to recover and try again."
        onUnblock={vi.fn()}
      />,
    )
    expect(
      screen.getByText(/anthropic API unreachable/i),
    ).toBeInTheDocument()
    expect(
      screen.getByText(/wait for the underlying service/i),
    ).toBeInTheDocument()
  })

  it('exposes itself as an alert region for screen readers', () => {
    render(
      <BlockerCard
        reason="something broke"
        recoveryHint="try again"
        onUnblock={vi.fn()}
      />,
    )
    const alert = screen.getByRole('alert')
    expect(alert).toHaveAttribute(
      'aria-label',
      'Conversation blocked — Something needs your attention before continuing',
    )
  })

  it('renders a variant-specific title when blockerKind is AgentError', () => {
    render(
      <BlockerCard
        reason="agent crashed"
        recoveryHint="retry"
        onUnblock={vi.fn()}
        blockerKind={{ kind: 'agent_error', message: 'agent crashed' }}
      />,
    )
    expect(
      screen.getByText(/The agent hit an error it couldn.t recover from/i),
    ).toBeInTheDocument()
    const alert = screen.getByRole('alert')
    expect(alert.getAttribute('data-blocker-kind')).toBe('agent_error')
  })

  it('renders a variant-specific title when blockerKind is MetricBelowThreshold', () => {
    render(
      <BlockerCard
        reason="silhouette = 0.12"
        recoveryHint="try a different k"
        onUnblock={vi.fn()}
        blockerKind={{
          kind: 'metric_below_threshold',
          metric: 'silhouette',
          threshold: 0.3,
          actual: 0.12,
        }}
      />,
    )
    expect(
      screen.getByText(/metric landed below the acceptable threshold/i),
    ).toBeInTheDocument()
    expect(
      screen.getByRole('button', { name: /Accept and continue/i }),
    ).toBeInTheDocument()
  })

  it('renders the AwaitingSmeSelection button label', () => {
    render(
      <BlockerCard
        reason="pick one of three clustering resolutions"
        recoveryHint="use the panel on the right"
        onUnblock={vi.fn()}
        blockerKind={{
          kind: 'awaiting_sme_selection',
          stage_id: 'clustering',
          candidates: ['0.4', '0.8', '1.2'],
        }}
      />,
    )
    expect(
      screen.getByRole('button', { name: /Continue once selection is made/i }),
    ).toBeInTheDocument()
    const alert = screen.getByRole('alert')
    expect(alert.getAttribute('data-blocker-kind')).toBe('awaiting_sme_selection')
  })

  it('fires onUnblock when the SME clicks the recovery button', async () => {
    const user = userEvent.setup()
    const onUnblock = vi.fn().mockResolvedValue(undefined)
    render(
      <BlockerCard
        reason="test reason"
        recoveryHint="test hint"
        onUnblock={onUnblock}
      />,
    )
    await user.click(
      screen.getByRole('button', { name: /addressed this — continue/i }),
    )
    expect(onUnblock).toHaveBeenCalledOnce()
  })

  it('disables the recovery button when disabled prop is set', () => {
    render(
      <BlockerCard
        reason="test"
        recoveryHint="test"
        onUnblock={vi.fn()}
        disabled
      />,
    )
    expect(
      screen.getByRole('button', { name: /addressed this — continue/i }),
    ).toBeDisabled()
  })
})

// Exhaustiveness — every BlockerKind variant renders a title

describe('BlockerCard — every BlockerKind variant renders', () => {
  // One entry per BlockerKind variant. tsc exhaustiveness on the
  // underlying switches is the real gate; this test is a runtime sanity
  // check that each variant produces a non-empty title on render.
  const variants: Array<{ name: string; kind: import('../types/BlockerKind').BlockerKind }> = [
    { name: 'data_shape_mismatch', kind: { kind: 'data_shape_mismatch', expected: 'matrix', actual: 'list' } },
    { name: 'validation_failed', kind: { kind: 'validation_failed', check: 'schema', message: 'bad', cause: null } },
    { name: 'metric_below_threshold', kind: { kind: 'metric_below_threshold', metric: 's', threshold: 0.3, actual: 0.1 } },
    { name: 'missing_input', kind: { kind: 'missing_input', dependency: 'upstream' } },
    { name: 'agent_error', kind: { kind: 'agent_error', message: 'crashed' } },
    { name: 'host_error', kind: { kind: 'host_error', message: 'net' } },
    { name: 'awaiting_sme_selection', kind: { kind: 'awaiting_sme_selection', stage_id: 's', candidates: ['a'] } },
    { name: 'pilot_oversize', kind: { kind: 'pilot_oversize', projected_usd: 50, ceiling_usd: 20 } },
    {
      name: 'stalled',
      kind: {
        kind: 'stalled',
        task_id: 't',
        signal: { kind: 'cpu_starvation', avg_cpu_pct: 2, window_mins: 30n },
        suggested_action: 'retry',
      },
    },
  ]

  for (const v of variants) {
    it(`renders with blockerKind=${v.name}`, () => {
      render(
        <BlockerCard
          reason="test"
          recoveryHint="test"
          onUnblock={vi.fn()}
          blockerKind={v.kind}
        />,
      )
      const alert = screen.getByRole('alert')
      expect(alert.getAttribute('data-blocker-kind')).toBe(v.name)
      // Any variant must produce a non-empty aria-label title.
      expect(alert.getAttribute('aria-label') ?? '').not.toBe(
        'Conversation blocked — ',
      )
    })
  }
})

/// `CompositionInfeasibleCard` surfaces the structured
/// `(unreachable_goal, missing_inputs, excluded_paths)` triple when
/// the composer can't reach the SME's goal from the registry. The
/// dedicated card replaces the generic free-text BlockerCard summary
/// with EDAM-typed missing-input affordances.
describe('CompositionInfeasibleCard structured detail', () => {
  it('renders unreachable goal + missing inputs + excluded paths', () => {
    render(
      <BlockerCard
        reason="composer infeasible"
        recoveryHint="Open composer recovery"
        onUnblock={vi.fn()}
        blockerKind={{
          kind: 'composition_infeasible',
          unreachable_goal: 'data:0951',
          missing_inputs: ['data:2044', 'format:1930'],
          excluded_paths: [
            { atom_id: 'star_align', exclusion_cel: 'intake.is_long_read' },
          ],
        }}
      />,
    )
    const region = screen.getByRole('region', {
      name: 'Composer recovery details',
    })
    expect(region).toBeTruthy()
    expect(region.textContent).toContain('Unreachable goal')
    expect(region.textContent).toContain('data:0951')
    expect(region.textContent).toContain('data:2044')
    expect(region.textContent).toContain('format:1930')
    expect(region.textContent).toContain('star_align')
    expect(region.textContent).toContain('intake.is_long_read')
  })

  it('renders nothing when all three fields are empty', () => {
    const { queryByRole } = render(
      <BlockerCard
        reason="composer infeasible"
        recoveryHint="Open composer recovery"
        onUnblock={vi.fn()}
        blockerKind={{
          kind: 'composition_infeasible',
          missing_inputs: [],
          excluded_paths: [],
        }}
      />,
    )
    expect(
      queryByRole('region', { name: 'Composer recovery details' }),
    ).toBeNull()
  })
})

// Discovery-approval blocker variant

describe('BlockerCard — discovery-approval variant', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  const reasonWithDecision =
    'Awaiting SME approval for normalization. Top candidate: vst (score 0.91). Runner-ups: tmm (0.82), cpm (0.71). Rationale: best-practice scorer pick. Full decision: runtime/outputs/discover_normalization/decision.json'

  const decisionBody = {
    task_id: 'discover_normalization',
    top_candidate: 'vst',
    runner_ups: ['tmm', 'cpm'],
    scores: { vst: 0.91, tmm: 0.82, cpm: 0.71 },
    rationale: 'best-practice scorer pick',
    auto_picked: false,
  }

  it('fetches decision.json when the reason references it + sessionId is set', async () => {
    mockFetch([jsonResponse(200, decisionBody)])
    render(
      <BlockerCard
        reason={reasonWithDecision}
        recoveryHint="pick a method"
        onUnblock={vi.fn()}
        sessionId="s1"
      />,
    )
    await waitFor(() =>
      expect(
        screen.getByLabelText('Candidates for discover_normalization'),
      ).toBeInTheDocument(),
    )
    expect(screen.getByLabelText('vst')).toBeChecked()
    expect(screen.getByLabelText('tmm')).not.toBeChecked()
    expect(screen.getByLabelText('cpm')).not.toBeChecked()
    // Chip renamed from bare "TOP" (substring-collides with
    // "Top candidate:" in the reason prose) to "★ RECOMMENDED".
    expect(
      screen.getByText('★ RECOMMENDED', { exact: true }),
    ).toBeInTheDocument()
    expect(
      screen.getByLabelText("Agent's recommended candidate"),
    ).toBeInTheDocument()
    expect(screen.getByText(/score 0\.91/)).toBeInTheDocument()
    expect(screen.getByText(/best-practice scorer pick/)).toBeInTheDocument()
  })

  it('degrades to plain-text reason when sessionId is null', () => {
    render(
      <BlockerCard
        reason={reasonWithDecision}
        recoveryHint="pick a method"
        onUnblock={vi.fn()}
        sessionId={null}
      />,
    )
    // No radio list, no fetch fired.
    expect(
      screen.queryByLabelText('Candidates for discover_normalization'),
    ).toBeNull()
    // The SME-visible reason has been sanitized — the runtime-path
    // fragment collapses to "the result file", stage IDs survive here
    // because the sanitizer only rewrites the `discover_` / `validate_`
    // / `select_` token boundaries, and the lead-in prose is preserved.
    expect(
      screen.getByText(/Awaiting SME approval for normalization/),
    ).toBeInTheDocument()
    expect(screen.getByText(/Full decision: the result file/)).toBeInTheDocument()
  })

  it('degrades when decision.json returns 404 (non-discovery blocker)', async () => {
    mockFetch([jsonResponse(404, 'not found')])
    render(
      <BlockerCard
        reason={reasonWithDecision}
        recoveryHint="pick a method"
        onUnblock={vi.fn()}
        sessionId="s1"
      />,
    )
    // Give the fetch a tick to settle.
    await waitFor(() =>
      expect(
        screen.queryByLabelText('Candidates for discover_normalization'),
      ).toBeNull(),
    )
    expect(
      screen.getByText(/Awaiting SME approval for normalization/),
    ).toBeInTheDocument()
    expect(screen.getByText(/Full decision: the result file/)).toBeInTheDocument()
  })

  it('POSTs sme-selection.json when the top candidate is accepted', async () => {
    // Regression guard: accepting the top candidate previously
    // skipped the sme-selection POST, so agent-claude.sh's approval
    // check (which requires runtime/outputs/<task>/sme-selection.json)
    // saw no SME input and re-blocked the task identically. Observed in
    // the IVD discover_annotation task — the SME had to pick a
    // runner-up to force the POST + unblock the loop. The fix: always
    // persist the chosen candidate on Accept, even when it equals the
    // top.
    const fetchMock = mockFetch([
      jsonResponse(200, decisionBody),
      jsonResponse(404, 'no blocker.json'),
      new Response(null, { status: 204 }),
    ])
    const onUnblock = vi.fn().mockResolvedValue(undefined)
    render(
      <BlockerCard
        reason={reasonWithDecision}
        recoveryHint="pick"
        onUnblock={onUnblock}
        sessionId="s1"
      />,
    )
    await waitFor(() => expect(screen.getByLabelText('vst')).toBeChecked())
    const user = userEvent.setup()
    await user.click(
      screen.getByRole('button', { name: /Accept selection and continue/i }),
    )
    expect(onUnblock).toHaveBeenCalledOnce()
    // decision.json + blocker.json fetches + sme-selection POST.
    expect(fetchMock).toHaveBeenCalledTimes(3)
    const selCall = fetchMock.mock.calls[2]
    expect(selCall![0]).toBe(
      '/api/v1/chat/session/s1/task/discover_normalization/sme-selection',
    )
    expect(selCall![1].method).toBe('POST')
    expect(JSON.parse(selCall![1].body as string)).toEqual({ chosen: 'vst' })
  })

  it('POSTs sme-selection.json when a runner-up is chosen', async () => {
    const fetchMock = mockFetch([
      jsonResponse(200, decisionBody),
      jsonResponse(404, 'no blocker.json'),
      new Response(null, { status: 204 }),
    ])
    const onUnblock = vi.fn().mockResolvedValue(undefined)
    render(
      <BlockerCard
        reason={reasonWithDecision}
        recoveryHint="pick"
        onUnblock={onUnblock}
        sessionId="s1"
      />,
    )
    await waitFor(() => expect(screen.getByLabelText('tmm')).toBeInTheDocument())
    const user = userEvent.setup()
    await user.click(screen.getByLabelText('tmm'))
    await user.click(
      screen.getByRole('button', { name: /Accept selection and continue/i }),
    )
    expect(onUnblock).toHaveBeenCalledOnce()
    // Third fetch call: the sme-selection POST.
    // (1=decision GET, 2=blocker GET, 3=sme-selection POST)
    const selCall = fetchMock.mock.calls[2]
    expect(selCall![0]).toBe(
      '/api/v1/chat/session/s1/task/discover_normalization/sme-selection',
    )
    expect(selCall![1].method).toBe('POST')
    expect(JSON.parse(selCall![1].body as string)).toEqual({ chosen: 'tmm' })
  })

  it('POSTs auto-approve-discoveries when the session checkbox is set', async () => {
    // Four fetches expected (in order): decision.json GET, blocker.json
    // GET, sme-selection POST (top candidate always persisted now),
    // and auto-approve-discoveries POST.
    const fetchMock = mockFetch([
      jsonResponse(200, decisionBody),
      jsonResponse(404, 'no blocker.json'),
      new Response(null, { status: 204 }),
      new Response(null, { status: 204 }),
    ])
    const onUnblock = vi.fn().mockResolvedValue(undefined)
    render(
      <BlockerCard
        reason={reasonWithDecision}
        recoveryHint="pick"
        onUnblock={onUnblock}
        sessionId="s1"
      />,
    )
    await waitFor(() => expect(screen.getByLabelText('vst')).toBeChecked())
    const user = userEvent.setup()
    await user.click(
      screen.getByLabelText(
        'Auto-approve routine discoveries (not integration, annotation, DE, or validation)',
      ),
    )
    await user.click(
      screen.getByRole('button', { name: /Accept selection and continue/i }),
    )
    expect(onUnblock).toHaveBeenCalledOnce()
    const selCall = fetchMock.mock.calls[2]
    expect(selCall![0]).toBe(
      '/api/v1/chat/session/s1/task/discover_normalization/sme-selection',
    )
    const autoCall = fetchMock.mock.calls[3]
    expect(autoCall![0]).toBe('/api/v1/chat/session/s1/auto-approve-discoveries')
    expect(autoCall![1].method).toBe('POST')
  })

  it('overrides decision.top_candidate with blocker.json hybrid fetcher half', async () => {
    // Regression: Run #7 misclassified data acquisition because
    // decision.json picked sme_supplied_local_path (covers 2/12
    // accessions, composite 0.935 with sme_input_boost) while
    // blocker.json correctly recommended a hybrid pairing the local
    // path with the multi-repo fetcher (composite of 0.87, covers
    // 12/12). The BlockerCard rubber-stamped decision.top_candidate
    // and dropped 10 of 12 studies. With the override in place,
    // chosen flips to the fetcher half (multi_repo_processed_matrices_python)
    // — the local-path coverage is still automatic via runtime/inputs.json,
    // so the agent uses both data sources and the cohort stays whole.
    const ivdDecision = {
      task_id: 'discover_data_acquisition',
      top_candidate: 'sme_supplied_local_path',
      runner_ups: ['multi_repo_processed_matrices_python', 'sme_provided_direct_urls'],
      scores: {
        sme_supplied_local_path: 0.935,
        multi_repo_processed_matrices_python: 0.87,
      },
      rationale: 'local-path top-scored but covers 2/12 accessions',
      auto_picked: false,
    }
    const ivdBlocker = {
      blocker_kind: 'awaiting_sme_approval',
      stage_class: 'data_acquisition',
      top_candidate:
        'hybrid:sme_supplied_local_path+multi_repo_processed_matrices_python',
      top_candidate_components: [
        {
          method_id: 'sme_supplied_local_path',
          score: 0.935,
          covers_accessions: ['CNP0002664', 'GSE230808'],
        },
        {
          method_id: 'multi_repo_processed_matrices_python',
          score: 0.87,
          covers_accessions: [
            'GSE160756',
            'GSE165722',
            'GSE189916',
            'GSE199866',
            'GSE205535',
            'GSE229711',
            'GSE233666',
            'GSE242443',
            'GSE244889',
            'GSE251686',
            'GSE255768',
          ],
        },
      ],
    }
    const ivdReason =
      'Awaiting SME approval for data_acquisition. Top candidate: sme_supplied_local_path (score 0.935). Full decision: runtime/outputs/discover_data_acquisition/decision.json'
    const fetchMock = mockFetch([
      jsonResponse(200, ivdDecision),
      jsonResponse(200, { blocker: ivdBlocker }),
      new Response(null, { status: 204 }),
    ])
    const onUnblock = vi.fn().mockResolvedValue(undefined)
    render(
      <BlockerCard
        reason={ivdReason}
        recoveryHint="pick"
        onUnblock={onUnblock}
        sessionId="s1"
      />,
    )
    // Wait for the radio panel to mount (proves decision.json loaded).
    await waitFor(() =>
      expect(
        screen.getByLabelText('Candidates for discover_data_acquisition'),
      ).toBeInTheDocument(),
    )
    const user = userEvent.setup()
    await user.click(
      screen.getByRole('button', { name: /Accept selection and continue/i }),
    )
    expect(onUnblock).toHaveBeenCalledOnce()
    // POST body must carry the FETCHER half, not the local-path half.
    const selCall = fetchMock.mock.calls[2]
    expect(selCall![0]).toBe(
      '/api/v1/chat/session/s1/task/discover_data_acquisition/sme-selection',
    )
    expect(JSON.parse(selCall![1].body as string)).toEqual({
      chosen: 'multi_repo_processed_matrices_python',
    })
  })

  it('falls back to decision.top_candidate when blocker.json has no hybrid breakdown', async () => {
    // Single-component or absent top_candidate_components leaves the
    // decision.json choice intact — important so the override doesn't
    // accidentally rewrite single-method picks (e.g., normalization
    // method choice between vst/tmm/cpm where there's no coverage
    // axis at play).
    const blockerNoComponents = {
      blocker_kind: 'awaiting_sme_approval',
      summary: 'pick a normalization method',
      // Only one component → no hybrid → decision.top_candidate wins.
      top_candidate: 'vst',
      top_candidate_components: [{ method_id: 'vst', score: 0.91 }],
    }
    const fetchMock = mockFetch([
      jsonResponse(200, decisionBody),
      jsonResponse(200, { blocker: blockerNoComponents }),
      new Response(null, { status: 204 }),
    ])
    render(
      <BlockerCard
        reason={reasonWithDecision}
        recoveryHint="pick"
        onUnblock={vi.fn().mockResolvedValue(undefined)}
        sessionId="s1"
      />,
    )
    await waitFor(() => expect(screen.getByLabelText('vst')).toBeChecked())
    const user = userEvent.setup()
    await user.click(
      screen.getByRole('button', { name: /Accept selection and continue/i }),
    )
    const selCall = fetchMock.mock.calls[2]
    expect(JSON.parse(selCall![1].body as string)).toEqual({ chosen: 'vst' })
  })
})

// Stall blocker three-button path

describe('BlockerCard — stall blocker variant', () => {
  const stallMemoryBlocker = {
    kind: 'stalled' as const,
    task_id: 'align-1',
    signal: {
      kind: 'memory_pressure' as const,
      pct: 93.4,
      window_mins: 5n,
    },
    suggested_action: 'resize' as const,
  }

  it('renders three resolution buttons (resize / retry / abort) for a stalled blocker', () => {
    render(
      <BlockerCard
        reason="stalled"
        recoveryHint="pick a recovery action"
        onUnblock={vi.fn()}
        blockerKind={stallMemoryBlocker}
      />,
    )
    expect(
      document.querySelector('[data-resolution="resize"]'),
    ).not.toBeNull()
    expect(
      document.querySelector('[data-resolution="retry"]'),
    ).not.toBeNull()
    expect(
      document.querySelector('[data-resolution="abort"]'),
    ).not.toBeNull()
  })

  it('marks the signal.suggested_action button with data-default-resolution="true"', () => {
    render(
      <BlockerCard
        reason="stalled"
        recoveryHint="pick a recovery action"
        onUnblock={vi.fn()}
        blockerKind={stallMemoryBlocker}
      />,
    )
    const resize = document.querySelector('[data-resolution="resize"]')
    expect(resize?.getAttribute('data-default-resolution')).toBe('true')
    const retry = document.querySelector('[data-resolution="retry"]')
    expect(retry?.getAttribute('data-default-resolution')).toBe('false')
    const abort = document.querySelector('[data-resolution="abort"]')
    expect(abort?.getAttribute('data-default-resolution')).toBe('false')
  })

  it('fires onUnblock with the matching resolution arg for each button', async () => {
    const user = userEvent.setup()
    const onUnblock = vi.fn().mockResolvedValue(undefined)
    const { rerender } = render(
      <BlockerCard
        reason="stalled"
        recoveryHint="pick"
        onUnblock={onUnblock}
        blockerKind={stallMemoryBlocker}
      />,
    )
    await user.click(
      document.querySelector('[data-resolution="resize"]') as HTMLElement,
    )
    expect(onUnblock).toHaveBeenCalledWith('resize')

    onUnblock.mockClear()
    rerender(
      <BlockerCard
        reason="stalled"
        recoveryHint="pick"
        onUnblock={onUnblock}
        blockerKind={stallMemoryBlocker}
      />,
    )
    await user.click(
      document.querySelector('[data-resolution="retry"]') as HTMLElement,
    )
    expect(onUnblock).toHaveBeenCalledWith('retry')

    onUnblock.mockClear()
    rerender(
      <BlockerCard
        reason="stalled"
        recoveryHint="pick"
        onUnblock={onUnblock}
        blockerKind={stallMemoryBlocker}
      />,
    )
    await user.click(
      document.querySelector('[data-resolution="abort"]') as HTMLElement,
    )
    expect(onUnblock).toHaveBeenCalledWith('abort')
  })
})

// ── Structured-decision path ──────────────────────────────────────────────

describe('BlockerCard — structured-decision variant', () => {
  const runtimeCapMissingKind = {
    kind: 'runtime_capability_missing' as const,
    sme_pinned_method: 'fgsea_msigdb_hallmark_reactome',
    missing_capability: 'r_fgsea',
    recommended_substitute: 'gseapy',
  }
  const blockerBody = {
    blocker: {
      blocker_kind: 'runtime_substitution',
      sme_pinned_method: 'fgsea_msigdb_hallmark_reactome',
      missing_capability: 'r_fgsea',
      recommended_substitute: 'gseapy',
      decision_points_for_sme: [
        {
          id: 'interp_runtime',
          question: 'R fgsea is missing. How should we proceed?',
          options: [
            {
              id: 'authorise_gseapy',
              label: 'Use gseapy (Python port, same algorithm)',
              risk: 'minimal',
            },
            {
              id: 'install_r_fgsea',
              label: 'Install R fgsea toolchain and retry',
              risk: 'deployment downtime',
            },
            {
              id: 'defer',
              label: 'Skip interpretation this pass',
              risk: 'no pathway results',
            },
          ],
          default_if_unanswered: 'authorise_gseapy',
        },
      ],
    },
  }

  const reason =
    "R fgsea missing — see runtime/outputs/biological_interpretation/blocker.json"

  it('fetches blocker.json, renders decision points, pre-selects the default', async () => {
    mockFetch([jsonResponse(200, blockerBody)])
    render(
      <BlockerCard
        reason={reason}
        recoveryHint="Authorise the substitute or install fgsea."
        onUnblock={vi.fn()}
        sessionId="s1"
        blockerKind={runtimeCapMissingKind}
      />,
    )
    // Fieldset appears for the decision_point.
    await waitFor(() =>
      expect(
        document.querySelector('[data-decision-point-id="interp_runtime"]'),
      ).not.toBeNull(),
    )
    // Three radio options render with stable data-option-id attributes.
    expect(
      document.querySelector('[data-option-id="authorise_gseapy"]'),
    ).not.toBeNull()
    expect(
      document.querySelector('[data-option-id="install_r_fgsea"]'),
    ).not.toBeNull()
    expect(
      document.querySelector('[data-option-id="defer"]'),
    ).not.toBeNull()
    // Default option gets the badge.
    expect(screen.getByTestId('default-option-badge')).toBeInTheDocument()
    // The default is pre-selected.
    const radio = document.querySelector<HTMLInputElement>(
      'input[value="authorise_gseapy"]',
    )
    expect(radio?.checked).toBe(true)
  })

  it('surfaces RuntimeCapabilityMissing typed fields in the intro copy', async () => {
    mockFetch([jsonResponse(200, blockerBody)])
    render(
      <BlockerCard
        reason={reason}
        recoveryHint="Authorise the substitute or install fgsea."
        onUnblock={vi.fn()}
        sessionId="s1"
        blockerKind={runtimeCapMissingKind}
      />,
    )
    // Method + capability + substitute all appear as inline code tags.
    await waitFor(() =>
      expect(
        screen.getByText('fgsea_msigdb_hallmark_reactome'),
      ).toBeInTheDocument(),
    )
    expect(screen.getByText('r_fgsea')).toBeInTheDocument()
    expect(screen.getByText('gseapy')).toBeInTheDocument()
  })

  it('POSTs sme-decisions and then unblocks when Apply is clicked', async () => {
    const fetchMock = mockFetch([
      jsonResponse(200, blockerBody),
      new Response(
        JSON.stringify({ written: true, decisions_count: 1 }),
        { status: 200, headers: { 'Content-Type': 'application/json' } },
      ),
    ])
    const onUnblock = vi.fn().mockResolvedValue(undefined)
    render(
      <BlockerCard
        reason={reason}
        recoveryHint="Authorise the substitute or install fgsea."
        onUnblock={onUnblock}
        sessionId="s1"
        blockerKind={runtimeCapMissingKind}
      />,
    )
    await waitFor(() =>
      expect(
        document.querySelector('[data-decision-point-id="interp_runtime"]'),
      ).not.toBeNull(),
    )
    const user = userEvent.setup()
    await user.click(
      screen.getByRole('button', { name: /Apply decision and continue/i }),
    )
    expect(onUnblock).toHaveBeenCalledOnce()
    // Second fetch was the sme-decisions POST.
    expect(fetchMock).toHaveBeenCalledTimes(2)
    const [url, init] = fetchMock.mock.calls[1]!
    expect(url).toBe(
      '/api/v1/chat/session/s1/task/biological_interpretation/sme-decisions',
    )
    expect((init as RequestInit).method).toBe('POST')
    const body = JSON.parse((init as RequestInit).body as string)
    expect(body.task_id).toBe('biological_interpretation')
    expect(body.decisions).toEqual([
      { id: 'interp_runtime', chosen: 'authorise_gseapy' },
    ])
  })

  it('lets the SME pick a non-default option and posts that choice instead', async () => {
    const fetchMock = mockFetch([
      jsonResponse(200, blockerBody),
      new Response(JSON.stringify({ written: true, decisions_count: 1 }), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      }),
    ])
    const onUnblock = vi.fn().mockResolvedValue(undefined)
    render(
      <BlockerCard
        reason={reason}
        recoveryHint="pick"
        onUnblock={onUnblock}
        sessionId="s1"
        blockerKind={runtimeCapMissingKind}
      />,
    )
    await waitFor(() =>
      expect(
        document.querySelector('[data-option-id="install_r_fgsea"]'),
      ).not.toBeNull(),
    )
    const user = userEvent.setup()
    await user.click(
      document.querySelector<HTMLElement>(
        '[data-option-id="install_r_fgsea"] input[type="radio"]',
      ) as HTMLElement,
    )
    await user.click(
      screen.getByRole('button', { name: /Apply decision and continue/i }),
    )
    const body = JSON.parse(
      (fetchMock!.mock.calls[1]![1] as RequestInit).body as string,
    )
    expect(body.decisions).toEqual([
      { id: 'interp_runtime', chosen: 'install_r_fgsea' },
    ])
  })

  it('forwards a rationale into the POST body when the SME fills it in', async () => {
    const fetchMock = mockFetch([
      jsonResponse(200, blockerBody),
      new Response(JSON.stringify({ written: true, decisions_count: 1 }), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      }),
    ])
    render(
      <BlockerCard
        reason={reason}
        recoveryHint="pick"
        onUnblock={vi.fn().mockResolvedValue(undefined)}
        sessionId="s1"
        blockerKind={runtimeCapMissingKind}
      />,
    )
    await waitFor(() =>
      expect(screen.getByTestId('structured-rationale')).toBeInTheDocument(),
    )
    const user = userEvent.setup()
    await user.type(
      screen.getByTestId('structured-rationale'),
      'gseapy matches the pydeseq2 pattern.',
    )
    await user.click(
      screen.getByRole('button', { name: /Apply decision and continue/i }),
    )
    const body = JSON.parse(
      (fetchMock!.mock.calls[1]![1] as RequestInit).body as string,
    )
    expect(body.rationale).toBe('gseapy matches the pydeseq2 pattern.')
  })

  it('routes via reason-regex even when blockerKind is DataShapeMismatch (legacy fallback)', async () => {
    mockFetch([jsonResponse(200, blockerBody)])
    const dataShapeKind = {
      kind: 'data_shape_mismatch' as const,
      expected: 'see blocker detail',
      actual: reason,
    }
    render(
      <BlockerCard
        reason={reason}
        recoveryHint="…"
        onUnblock={vi.fn()}
        sessionId="s1"
        blockerKind={dataShapeKind}
      />,
    )
    // The structured picker still renders because the reason text
    // references blocker.json, covering sessions emitted against an
    // older server that hadn't run through the new mapper.
    await waitFor(() =>
      expect(
        document.querySelector('[data-decision-point-id="interp_runtime"]'),
      ).not.toBeNull(),
    )
  })

  it('surfaces a related-disposition hint when relatedDispositionPath is set', () => {
    render(
      <BlockerCard
        reason="Upstream output missing"
        recoveryHint="…"
        onUnblock={vi.fn()}
        blockerKind={{
          kind: 'missing_artifact',
          task_id: 'trajectory_analysis',
          missing_paths: ['results/tables/umap.tsv'],
        }}
        relatedDispositionPath="runtime/outputs/results_review/sme_disposition.json"
        relatedDispositionTaskId="results_review"
      />,
    )
    const hint = screen.getByTestId('related-disposition-hint')
    expect(hint).toHaveTextContent(/unapplied disposition/i)
    expect(hint).toHaveTextContent(/results_review/)
  })

  it('does NOT render the hint when relatedDispositionPath is absent', () => {
    render(
      <BlockerCard
        reason="Upstream output missing"
        recoveryHint="…"
        onUnblock={vi.fn()}
        blockerKind={{
          kind: 'missing_artifact',
          task_id: 'trajectory_analysis',
          missing_paths: ['results/tables/umap.tsv'],
        }}
      />,
    )
    expect(
      screen.queryByTestId('related-disposition-hint'),
    ).not.toBeInTheDocument()
  })

  it('shows an empty-decision-points message when blocker.json has none', async () => {
    mockFetch([
      jsonResponse(200, {
        blocker: {
          blocker_kind: 'runtime_substitution',
          decision_points_for_sme: [],
        },
      }),
    ])
    render(
      <BlockerCard
        reason={reason}
        recoveryHint="…"
        onUnblock={vi.fn()}
        sessionId="s1"
        blockerKind={runtimeCapMissingKind}
      />,
    )
    await waitFor(() =>
      expect(screen.getByTestId('structured-blocker-empty')).toBeInTheDocument(),
    )
  })

  it('clicking Apply on an empty-decision-points blocker calls onUnblock without posting decisions', async () => {
    // One fetch fires on mount for a non-discover taskId: blocker.json
    // → empty decision_points_for_sme. The decision.json fetch is gated
    // on a discover_* prefix (it's a discovery-task artifact, so issuing
    // a request for `clustering` would just 404-noise the console).
    const fetchMock = mockFetch([
      jsonResponse(200, {
        blocker: {
          blocker_kind: 'missing_artifact',
          decision_points_for_sme: [],
        },
      }),
    ])
    const onUnblock = vi.fn().mockResolvedValue(undefined)
    render(
      <BlockerCard
        reason="A required output file is missing or empty"
        recoveryHint="…"
        onUnblock={onUnblock}
        sessionId="s1"
        taskId="clustering"
      />,
    )
    await waitFor(() =>
      expect(screen.getByTestId('structured-blocker-empty')).toBeInTheDocument(),
    )
    const button = screen.getByRole('button', {
      name: /apply decision and continue/i,
    })
    await userEvent.click(button)
    await waitFor(() => expect(onUnblock).toHaveBeenCalledTimes(1))
    // No second fetch (sme-decisions POST) — only the single blocker.json
    // read on mount (decision.json is gated to discover_* tasks).
    expect(fetchMock).toHaveBeenCalledTimes(1)
    expect(
      screen.queryByText(/no decisions chosen/i),
    ).not.toBeInTheDocument()
  })

  it('keeps the submit button anchored outside the scrollable body so a tall structured blocker stays clickable', async () => {
    mockFetch([
      jsonResponse(200, {
        blocker: {
          blocker_kind: 'runtime_substitution',
          decision_points_for_sme: [
            {
              id: 'dp1',
              question: 'Q1',
              options: [
                { id: 'a', label: 'A', risk: 'r', consequence: 'c' },
                { id: 'b', label: 'B', risk: 'r', consequence: 'c' },
              ],
            },
          ],
        },
      }),
    ])
    render(
      <BlockerCard
        reason={reason}
        recoveryHint="…"
        onUnblock={vi.fn()}
        sessionId="s1"
        blockerKind={runtimeCapMissingKind}
      />,
    )
    const body = await screen.findByTestId('blocker-card-body')
    const section = screen.getByRole('alert')
    const button = screen.getByRole('button', {
      name: /apply decision and continue/i,
    })
    // section is a bounded flex column
    expect(section.style.display).toBe('flex')
    expect(section.style.flexDirection).toBe('column')
    expect(section.style.maxHeight).toMatch(/min\(/)
    // body is the scrollable region
    expect(body.style.overflowY).toBe('auto')
    expect(body.style.minHeight).toBe('0')
    // button is a sibling of the body, not a descendant — guarantees it
    // is always visible regardless of how tall the body grows
    expect(body.contains(button)).toBe(false)
    expect(button.parentElement).toBe(section)
  })

  /// When the parent surface (e.g. TaskDetailDrawer) already knows
  /// which task is being viewed, it can supply the taskId prop to
  /// force structured-decision rendering even when the reason text
  /// doesn't reference blocker.json and no typed blockerKind is
  /// available. The picker then fetches blocker.json for the named
  /// task and surfaces its decision_points_for_sme.
  it('renders structured decision picker when explicit taskId prop is supplied', async () => {
    // For a non-discover taskId ("batch_correction") only blocker.json
    // is fetched — the decision.json effect short-circuits on the
    // discover_* prefix check.
    mockFetch([
      jsonResponse(200, {
        blocker: {
          blocker_kind: 'awaiting_sme_approval',
          decision_points_for_sme: [
            {
              id: 'cca_scale_strategy',
              question: 'Pick scale strategy',
              options: [
                { id: 'reference', label: 'Reference-based' },
                { id: 'per_compartment', label: 'Per-compartment' },
              ],
              default_if_unanswered: 'reference',
            },
          ],
        },
        attempts: [],
      }),
    ])
    render(
      <BlockerCard
        reason="Awaiting SME approval — see runtime details."
        recoveryHint=""
        onUnblock={vi.fn()}
        sessionId="s1"
        taskId="batch_correction"
      />,
    )
    // The structured picker fetches blocker.json for the named task
    // (verified by the answer + radio appearing) — without the
    // taskId prop the same render would fall through to the plain
    // reason+recovery body.
    await waitFor(() =>
      expect(screen.getByText(/Pick scale strategy/i)).toBeInTheDocument(),
    )
    expect(
      screen.getByRole('button', { name: /apply decision and continue/i }),
    ).toBeInTheDocument()
  })
})

// Typed BlockerKind::SandboxRefused dispatch. The harness
// emits `[sandbox_refused] <pieces>` strings; the server upgrades them
// to a typed BlockerKind via `parse_agent_blocker_kind`; the UI's
// dispatch table must render the per-refusal list as a structured
// section instead of falling through to the generic blocker render.
describe('BlockerCard — sandbox_refused variant', () => {
  it('renders SandboxRefused refusals', () => {
    render(
      <BlockerCard
        reason="[sandbox_refused] NetworkDenied: (node=t1); HostFsDenied: (node=t1)"
        recoveryHint="Adjust the active policy bundle."
        onUnblock={vi.fn()}
        blockerKind={{
          kind: 'sandbox_refused',
          refusals: [
            { kind: 'NetworkDenied', detail: 'task=t1 attempted egress to 1.2.3.4' },
            { kind: 'HostFsDenied', detail: '' },
          ],
        }}
      />,
    )
    // Title appears in the alert region's aria-label.
    const alert = screen.getByRole('alert')
    expect(alert.getAttribute('aria-label')).toMatch(/Sandbox refused/i)
    // The dispatch attribute reflects the typed variant.
    expect(alert.getAttribute('data-blocker-kind')).toBe('sandbox_refused')
    // Each refusal kind appears in the structured list.
    expect(screen.getByText('NetworkDenied')).toBeInTheDocument()
    expect(screen.getByText('HostFsDenied')).toBeInTheDocument()
    // The detail text follows the kind label when non-empty.
    expect(
      screen.getByText(/task=t1 attempted egress to 1\.2\.3\.4/),
    ).toBeInTheDocument()
    // The dedicated list section exists for screen readers + automation.
    expect(screen.getByTestId('sandbox-refused-list')).toBeInTheDocument()
  })
})

// Atom-safety-policy three new dispatch-time BlockerKinds
// from the harness `enforce_safety_policy` gate. Each routes to a
// structured panel with affordances tailored to the policy mismatch
// (sandbox upgrade, network policy reconciliation, package add).

describe('BlockerCard — sandbox_required variant', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('renders SandboxRequired blocker with switch-executor affordance', () => {
    render(
      <BlockerCard
        reason="atom interpret_results requires process_isolation sandbox"
        recoveryHint="Pick an executor that offers process_isolation."
        onUnblock={vi.fn()}
        blockerKind={{
          kind: 'sandbox_required',
          atom_id: 'interpret_results',
          requested: 'process_isolation',
          available: 'none',
        }}
      />,
    )
    const alert = screen.getByRole('alert')
    expect(alert.getAttribute('data-blocker-kind')).toBe('sandbox_required')
    expect(alert.getAttribute('aria-label')).toMatch(/sandbox/i)
    // The dedicated panel exists for screen readers + automation.
    const panel = screen.getByTestId('sandbox-required-detail')
    expect(panel).toBeInTheDocument()
    // Atom id surfaces in the dedicated panel so the SME knows which
    // task tripped the gate.
    expect(panel.textContent).toMatch(/interpret_results/)
    // Both the requested and the available sandbox levels appear.
    expect(panel.textContent).toMatch(/process_isolation/)
    expect(panel.textContent).toMatch(/none/)
    // The "Switch executor" affordance is rendered.
    expect(
      screen.getByRole('button', { name: /switch executor/i }),
    ).toBeInTheDocument()
  })

  it('dispatches ecaax:open-settings when the Switch-executor button is clicked', async () => {
    const listener = vi.fn()
    window.addEventListener('ecaax:open-settings', listener)
    try {
      render(
        <BlockerCard
          reason="atom interpret_results requires process_isolation sandbox"
          recoveryHint="Pick an executor that offers process_isolation."
          onUnblock={vi.fn()}
          blockerKind={{
            kind: 'sandbox_required',
            atom_id: 'interpret_results',
            requested: 'process_isolation',
            available: 'none',
          }}
        />,
      )
      const user = userEvent.setup()
      // The button uses a sandbox-suffixed test id so it doesn't
      // collide with the network-policy-mismatch button.
      const btn = screen.getByTestId('switch-executor-button-sandbox')
      await user.click(btn)
      expect(listener).toHaveBeenCalledTimes(1)
    } finally {
      window.removeEventListener('ecaax:open-settings', listener)
    }
  })
})

describe('BlockerCard — network_policy_mismatch variant', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('renders NetworkPolicyMismatch blocker with executor + amend options', () => {
    render(
      <BlockerCard
        reason="atom fetch_reference wants bridge but executor offers none"
        recoveryHint="Reconcile the network policies."
        onUnblock={vi.fn()}
        blockerKind={{
          kind: 'network_policy_mismatch',
          atom_id: 'fetch_reference',
          atom_network: { kind: 'bridge' },
          executor_network: { kind: 'none', allowlist: [] },
        }}
      />,
    )
    const alert = screen.getByRole('alert')
    expect(alert.getAttribute('data-blocker-kind')).toBe('network_policy_mismatch')
    expect(alert.getAttribute('aria-label')).toMatch(/network/i)
    // Both policy summaries appear in the dedicated panel.
    const panel = screen.getByTestId('network-policy-mismatch-detail')
    expect(panel).toBeInTheDocument()
    expect(panel.textContent).toMatch(/fetch_reference/)
    expect(panel.textContent).toMatch(/bridge/)
    expect(panel.textContent).toMatch(/no network/)
    expect(
      screen.getByRole('button', { name: /switch executor/i }),
    ).toBeInTheDocument()
    expect(
      screen.getByRole('button', { name: /amend atom safety/i }),
    ).toBeInTheDocument()
  })

  it('dispatches ecaax:open-settings when the Switch-executor button is clicked', async () => {
    const listener = vi.fn()
    window.addEventListener('ecaax:open-settings', listener)
    try {
      render(
        <BlockerCard
          reason="atom fetch_reference wants bridge but executor offers none"
          recoveryHint="Reconcile the network policies."
          onUnblock={vi.fn()}
          blockerKind={{
            kind: 'network_policy_mismatch',
            atom_id: 'fetch_reference',
            atom_network: { kind: 'bridge' },
            executor_network: { kind: 'none', allowlist: [] },
          }}
        />,
      )
      const user = userEvent.setup()
      const btn = screen.getByTestId('switch-executor-button-network')
      await user.click(btn)
      expect(listener).toHaveBeenCalledTimes(1)
    } finally {
      window.removeEventListener('ecaax:open-settings', listener)
    }
  })

  it('renders Amend-atom-safety as a disabled button with explanatory tooltip', () => {
    render(
      <BlockerCard
        reason="atom fetch_reference wants bridge but executor offers none"
        recoveryHint="Reconcile the network policies."
        onUnblock={vi.fn()}
        blockerKind={{
          kind: 'network_policy_mismatch',
          atom_id: 'fetch_reference',
          atom_network: { kind: 'bridge' },
          executor_network: { kind: 'none', allowlist: [] },
        }}
      />,
    )
    const btn = screen.getByTestId('amend-atom-safety-button') as HTMLButtonElement
    expect(btn).toBeDisabled()
    expect(btn.title).toMatch(/coming/i)
  })
})

describe('BlockerCard — provisioning_denied variant', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('renders ProvisioningDenied blocker with add-package affordance', () => {
    render(
      <BlockerCard
        reason="atom align_reads denied request for samtools (apt)"
        recoveryHint="Add samtools to atom.runtime_packages."
        onUnblock={vi.fn()}
        blockerKind={{
          kind: 'provisioning_denied',
          atom_id: 'align_reads',
          package: 'samtools',
          registry: 'apt',
        }}
      />,
    )
    const alert = screen.getByRole('alert')
    expect(alert.getAttribute('data-blocker-kind')).toBe('provisioning_denied')
    const panel = screen.getByTestId('provisioning-denied-detail')
    expect(panel).toBeInTheDocument()
    expect(panel.textContent).toMatch(/align_reads/)
    expect(panel.textContent).toMatch(/samtools/)
    expect(panel.textContent).toMatch(/apt/)
    expect(
      screen.getByRole('button', { name: /add `samtools`/i }),
    ).toBeInTheDocument()
  })

  it('POSTs add-runtime-package endpoint when the affordance is clicked', async () => {
    const fetchMock = mockFetch([new Response(null, { status: 204 })])
    render(
      <BlockerCard
        reason="atom align_reads denied samtools"
        recoveryHint=""
        onUnblock={vi.fn()}
        sessionId="s1"
        blockerKind={{
          kind: 'provisioning_denied',
          atom_id: 'align_reads',
          package: 'samtools',
          registry: 'apt',
        }}
      />,
    )
    const user = userEvent.setup()
    await user.click(screen.getByRole('button', { name: /add `samtools`/i }))
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1))
    const call = fetchMock.mock.calls[0]
    expect(call![0]).toBe(
      '/api/v1/chat/session/s1/atom/align_reads/add-runtime-package',
    )
    expect(call![1].method).toBe('POST')
    expect(JSON.parse(call![1].body as string)).toEqual({
      package: 'samtools',
      registry: 'apt',
    })
  })

  it('shows SME-safe "rolling out" copy when the endpoint 404s (Task 4.7 not landed)', async () => {
    mockFetch([new Response(null, { status: 404, statusText: 'Not Found' })])
    render(
      <BlockerCard
        reason="atom align_reads denied samtools"
        recoveryHint=""
        onUnblock={vi.fn()}
        sessionId="s1"
        blockerKind={{
          kind: 'provisioning_denied',
          atom_id: 'align_reads',
          package: 'samtools',
          registry: 'apt',
        }}
      />,
    )
    const user = userEvent.setup()
    await user.click(screen.getByRole('button', { name: /add `samtools`/i }))
    // Wait for the catch branch to set the status message.
    await waitFor(() =>
      expect(screen.getByRole('status').textContent).toMatch(/rolling out/i),
    )
    // The raw URL must not leak to the SME.
    expect(screen.getByRole('status').textContent).not.toMatch(/\/api\//)
    expect(screen.getByRole('status').textContent).not.toMatch(/404/)
  })
})
