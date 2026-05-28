import { describe, expect, it, vi } from 'vitest'
import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import ResultReviewTurnCard from './ResultReviewTurnCard'
import type { ResultReviewPayload } from './ResultReviewTurnCard'

describe('ResultReviewTurnCard', () => {
  const base: ResultReviewPayload = {
    task_id: 'clustering',
    status: 'completed',
    description: 'Leiden clustering at resolution 0.8',
    kind: { type: 'computation' } as never,
    result: { silhouette: 0.42, k: 14 },
  }

  it('renders the human-label derived from task_id, plus description and status badge', () => {
    render(<ResultReviewTurnCard payload={base} />)
    // Header renders the SME-readable label derived from task_id. The raw
    // id stays in the title attribute + data-task-id for debug/e2e.
    expect(screen.getByText('Clustering')).toBeInTheDocument()
    expect(screen.getByTitle('clustering')).toBeInTheDocument()
    expect(
      screen.getByText(/Leiden clustering at resolution 0.8/i),
    ).toBeInTheDocument()
    expect(screen.getByText('completed')).toBeInTheDocument()
  })

  it('pretty-prints the completed result JSON', () => {
    render(<ResultReviewTurnCard payload={base} />)
    const pre = screen.getByLabelText(/Task result JSON/i)
    expect(pre.textContent).toContain('"silhouette": 0.42')
    expect(pre.textContent).toContain('"k": 14')
  })

  it('shows a reason instead of JSON when status is failed', () => {
    render(
      <ResultReviewTurnCard
        payload={{
          ...base,
          status: 'failed',
          result: undefined,
          reason: 'star-2pass OOM-killed at iteration 3',
        }}
      />,
    )
    expect(screen.getByText(/star-2pass OOM-killed/i)).toBeInTheDocument()
    expect(screen.queryByLabelText(/Task result JSON/i)).not.toBeInTheDocument()
  })

  it('renders the blocked record as JSON when status is blocked', () => {
    render(
      <ResultReviewTurnCard
        payload={{
          ...base,
          status: 'blocked',
          result: undefined,
          record: { reason: 'missing input', dependency: 'qc' },
        }}
      />,
    )
    const pre = screen.getByLabelText(/Blocked record/i)
    expect(pre.textContent).toContain('"reason": "missing input"')
  })

  it('exposes data-result-status for e2e / axe selectors', () => {
    render(<ResultReviewTurnCard payload={base} />)
    const region = screen.getByRole('region', { name: /Result for task clustering/i })
    expect(region.getAttribute('data-result-status')).toBe('completed')
    expect(region.getAttribute('data-task-id')).toBe('clustering')
  })

  it('expands the rerun panel and invokes onRerun with task id + reason', async () => {
    const user = userEvent.setup()
    const onRerun = vi.fn().mockResolvedValue(undefined)
    render(<ResultReviewTurnCard payload={base} onRerun={onRerun} />)
    await user.click(screen.getByRole('button', { name: /Rerun this task/i }))
    // Panel open — the reason textarea should appear.
    await user.type(
      screen.getByLabelText(/Optional rerun reason/i),
      'inputs drifted',
    )
    await user.click(screen.getByRole('button', { name: /Confirm rerun/i }))
    expect(onRerun).toHaveBeenCalledWith('clustering', 'inputs drifted')
  })

  it('passes undefined reason when rerun textarea is blank', async () => {
    const user = userEvent.setup()
    const onRerun = vi.fn().mockResolvedValue(undefined)
    render(<ResultReviewTurnCard payload={base} onRerun={onRerun} />)
    await user.click(screen.getByRole('button', { name: /Rerun this task/i }))
    await user.click(screen.getByRole('button', { name: /Confirm rerun/i }))
    expect(onRerun).toHaveBeenCalledWith('clustering', undefined)
  })

  it('cancel button closes the rerun panel without dispatching', async () => {
    const user = userEvent.setup()
    const onRerun = vi.fn()
    render(<ResultReviewTurnCard payload={base} onRerun={onRerun} />)
    await user.click(screen.getByRole('button', { name: /Rerun this task/i }))
    await user.click(screen.getByRole('button', { name: /^Cancel$/i }))
    expect(onRerun).not.toHaveBeenCalled()
    expect(
      screen.queryByLabelText(/Optional rerun reason/i),
    ).not.toBeInTheDocument()
  })

  it('hides the rerun button when onRerun is not provided', () => {
    render(<ResultReviewTurnCard payload={base} />)
    expect(
      screen.queryByRole('button', { name: /Rerun this task/i }),
    ).not.toBeInTheDocument()
  })

  it('does not render a verification panel when none was returned', () => {
    render(<ResultReviewTurnCard payload={base} />)
    expect(
      screen.queryByLabelText(/Claim verification summary/i),
    ).not.toBeInTheDocument()
  })

  it('renders a clean-tone verification summary when every claim verified', () => {
    render(
      <ResultReviewTurnCard
        payload={{
          ...base,
          verification: {
            n_checked: 2,
            n_verified: 2,
            n_mismatch: 0,
            n_unverifiable: 0,
            verdicts: [
              {
                claim: {
                  entity: 'ACAN',
                  direction: 'up',
                  effect_size: 2.1,
                  pvalue: 0.001,
                  source_table: 'Table S1',
                  excerpt: 'ACAN was upregulated (Table S1).',
                  contract: 'numeric_table_lookup',
                },
                status: { status: 'verified' },
                strength: 'exploratory',
              },
              {
                claim: {
                  entity: 'COL2A1',
                  direction: 'down',
                  effect_size: -1.5,
                  pvalue: 0.003,
                  source_table: 'Table S1',
                  excerpt: 'COL2A1 was downregulated (Table S1).',
                  contract: 'numeric_table_lookup',
                },
                status: { status: 'verified' },
                strength: 'exploratory',
              },
            ],
          },
        }}
      />,
    )
    const panel = screen.getByLabelText(/Claim verification summary/i)
    expect(panel).toHaveAttribute('data-verification-tone', 'clean')
    expect(panel.textContent).toContain('2/2 verified')
  })

  it('shows mismatch tone + expandable detail when verifier flagged a claim', async () => {
    const user = userEvent.setup()
    render(
      <ResultReviewTurnCard
        payload={{
          ...base,
          verification: {
            n_checked: 2,
            n_verified: 1,
            n_mismatch: 1,
            n_unverifiable: 0,
            verdicts: [
              {
                claim: {
                  entity: 'ACAN',
                  direction: 'up',
                  effect_size: 2.1,
                  pvalue: 0.001,
                  source_table: 'Table S1',
                  excerpt: 'ACAN was upregulated (Table S1).',
                  contract: 'numeric_table_lookup',
                },
                status: {
                  status: 'mismatch',
                  detail: 'direction: narrative says Up, table effect size is -1.2',
                },
                strength: 'exploratory',
              },
              {
                claim: {
                  entity: 'COL2A1',
                  direction: 'down',
                  effect_size: -1.5,
                  pvalue: 0.003,
                  source_table: 'Table S1',
                  excerpt: 'COL2A1 was downregulated (Table S1).',
                  contract: 'numeric_table_lookup',
                },
                status: { status: 'verified' },
                strength: 'exploratory',
              },
            ],
          },
        }}
      />,
    )
    const panel = screen.getByLabelText(/Claim verification summary/i)
    expect(panel).toHaveAttribute('data-verification-tone', 'mismatch')
    expect(panel.textContent).toContain('1 mismatch')

    await user.click(screen.getByRole('button', { name: /Show detail/i }))
    expect(screen.getByText(/narrative says Up, table effect size is -1.2/)).toBeInTheDocument()
  })

  it('renders an empty-verification message when the report has zero claims', () => {
    render(
      <ResultReviewTurnCard
        payload={{
          ...base,
          verification: {
            n_checked: 0,
            n_verified: 0,
            n_mismatch: 0,
            n_unverifiable: 0,
            verdicts: [],
          },
        }}
      />,
    )
    expect(screen.getByText(/No verifiable claims/i)).toBeInTheDocument()
  })

  // Cross-version diff rendering

  const crossVersionFixture = {
    parent_package: '/pkgs/parent',
    child_package: '/pkgs/child',
    overall_concordance: 0.73,
    anchor_kind: 'package-pair',
    tables: [
      {
        table_name: 'deg',
        n_rows_parent: 125,
        n_rows_child: 128,
        n_overlap: 120,
        n_robust: 95,
        n_concordant: 20,
        n_discordant: 5,
        n_entity_missing: 0,
        n_numerics_incomplete: 0,
        rows: [],
      },
    ],
  }

  it('renders cross-version summary with robust/concordant/discordant counts + Open diff button', () => {
    render(
      <ResultReviewTurnCard
        payload={{
          ...base,
          cross_version_diff: crossVersionFixture,
        }}
        sessionId="sess-xv"
      />,
    )
    const summary = screen.getByLabelText('Cross-version diff summary')
    expect(summary).toBeInTheDocument()
    // Counts from the single fixture table.
    expect(summary.textContent).toContain('95 robust')
    expect(summary.textContent).toContain('20 concordant')
    expect(summary.textContent).toContain('5 discordant')
    // Discordant > 0 triggers the discordant tone.
    expect(summary.getAttribute('data-cross-version-tone')).toBe('discordant')
    // Open-diff button is present.
    expect(
      screen.getByRole('button', { name: /Open diff/i }),
    ).toBeInTheDocument()
  })

  // Convergence-trajectory chart wired in for iterate-until
  // tasks. The chart renders only when the result carries a metric_trail
  // array; otherwise the existing JSON dump is the only artifact.
  it('renders an iteration convergence chart when result has metric_trail', () => {
    render(
      <ResultReviewTurnCard
        payload={{
          ...base,
          task_id: 'iterate_check_clustering',
          description: 'Convergence check for clustering iteration loop',
          result: {
            iter_count: 4,
            converged_at: 4,
            last_metric: 0.04,
            threshold: 0.05,
            operator: '<',
            metric_trail: [
              { iter: 1, metric: 0.42 },
              { iter: 2, metric: 0.21 },
              { iter: 3, metric: 0.09 },
              { iter: 4, metric: 0.04 },
            ],
          },
        }}
      />,
    )
    expect(screen.getByText(/Convergence trajectory/i)).toBeInTheDocument()
    expect(screen.getByText(/Converged at iter 4/i)).toBeInTheDocument()
  })

  it('omits the convergence chart on non-iterate results', () => {
    // Silent additive surface. A normal completed result
    // (clustering / DE / etc.) has no metric_trail; the chart must not
    // render at all so non-iterate cards are unchanged.
    render(<ResultReviewTurnCard payload={base} />)
    expect(screen.queryByText(/Convergence trajectory/i)).not.toBeInTheDocument()
  })

  it('opens CrossVersionDiffCard modal when Open diff is clicked', async () => {
    // Stub fetch so the inner CrossVersionDiffCard's effect doesn't
    // blow up during the test — we just need the modal aria-label.
    const fetchMock = vi
      .spyOn(globalThis, 'fetch')
      .mockResolvedValue(
        new Response(JSON.stringify(crossVersionFixture), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        }),
      )
    const user = userEvent.setup()
    render(
      <ResultReviewTurnCard
        payload={{
          ...base,
          cross_version_diff: crossVersionFixture,
        }}
        sessionId="sess-xv"
      />,
    )
    // Before click — modal hidden.
    expect(
      screen.queryByRole('dialog', { name: /Cross-version diff/i }),
    ).not.toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: /Open diff/i }))
    // After click — modal visible with the expected aria-label.
    const dialog = screen.getByRole('dialog', { name: /Cross-version diff/i })
    expect(dialog).toBeInTheDocument()
    fetchMock.mockRestore()
  })
})
