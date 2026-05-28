import { describe, expect, it, vi } from 'vitest'
import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import SensitivityComparisonCard from './SensitivityComparisonCard'
import type { CrossVersionReport } from '../types'

describe('SensitivityComparisonCard', () => {
  it('renders a heading with the SME-readable stage label and a radio per candidate', () => {
    render(
      <SensitivityComparisonCard
        stage="compare_integration"
        candidates={['harmony', 'scanorama', 'bbknn']}
        onSelect={vi.fn()}
      />,
    )
    // Header uses the label derived from stageIdToLabel; the raw id stays
    // on the title attribute + data-stage-id for debug and e2e hooks.
    expect(screen.getByRole('heading', { level: 3 }).textContent).toContain(
      'Compare integration',
    )
    expect(screen.getByTitle('compare_integration')).toBeInTheDocument()
    expect(screen.getAllByRole('radio').length).toBe(3)
  })

  it('disables the submit button until a candidate is selected', async () => {
    const user = userEvent.setup()
    render(
      <SensitivityComparisonCard
        stage="compare_integration"
        candidates={['harmony', 'scanorama']}
        onSelect={vi.fn()}
      />,
    )
    const button = screen.getByRole('button', { name: /Record choice/i })
    expect(button).toBeDisabled()
    await user.click(screen.getByRole('radio', { name: /Select harmony/i }))
    expect(button).toBeEnabled()
  })

  it('dispatches onSelect with the picked winner and the rationale', async () => {
    const user = userEvent.setup()
    const onSelect = vi.fn().mockResolvedValue(undefined)
    render(
      <SensitivityComparisonCard
        stage="compare_integration"
        candidates={['harmony', 'scanorama']}
        onSelect={onSelect}
      />,
    )
    await user.click(screen.getByRole('radio', { name: /Select scanorama/i }))
    await user.type(
      screen.getByLabelText(/Optional rationale/i),
      'tightest silhouette',
    )
    await user.click(screen.getByRole('button', { name: /Record choice/i }))
    expect(onSelect).toHaveBeenCalledWith('scanorama', 'tightest silhouette')
  })

  it('passes undefined rationale when textarea is empty', async () => {
    const user = userEvent.setup()
    const onSelect = vi.fn().mockResolvedValue(undefined)
    render(
      <SensitivityComparisonCard
        stage="compare_integration"
        candidates={['harmony', 'bbknn']}
        onSelect={onSelect}
      />,
    )
    await user.click(screen.getByRole('radio', { name: /Select bbknn/i }))
    await user.click(screen.getByRole('button', { name: /Record choice/i }))
    expect(onSelect).toHaveBeenCalledWith('bbknn', undefined)
  })

  it('disables the whole card when disabled prop is set', async () => {
    render(
      <SensitivityComparisonCard
        stage="compare_integration"
        candidates={['harmony']}
        onSelect={vi.fn()}
        disabled
      />,
    )
    expect(screen.getByRole('button', { name: /Record choice/i })).toBeDisabled()
    expect(screen.getByRole('radio', { name: /Select harmony/i })).toBeDisabled()
    expect(screen.getByLabelText(/Optional rationale/i)).toBeDisabled()
  })

  it('renders an empty-state notice when no candidates are passed', () => {
    render(
      <SensitivityComparisonCard
        stage="compare_integration"
        candidates={[]}
        onSelect={vi.fn()}
      />,
    )
    expect(
      screen.getByText(/No candidates yet — results haven.t arrived/i),
    ).toBeInTheDocument()
  })

  it('exposes data-stage-id attribute for e2e hooks', () => {
    render(
      <SensitivityComparisonCard
        stage="compare_integration"
        candidates={['harmony']}
        onSelect={vi.fn()}
      />,
    )
    const region = screen.getByRole('region')
    expect(region.getAttribute('data-stage-id')).toBe('compare_integration')
  })

  it('renders cross-version concordance bars when crossVersion is supplied', () => {
    const report: CrossVersionReport = {
      parent_package: 'pkg-v1',
      child_package: 'pkg-v2',
      overall_concordance: 0.875,
      anchor_kind: 'package-pair',
      tables: [
        {
          table_name: 'de_results.tsv',
          n_rows_parent: 120,
          n_rows_child: 118,
          n_overlap: 116,
          n_robust: 60,
          n_concordant: 30,
          n_discordant: 10,
          n_entity_missing: 0,
          n_numerics_incomplete: 0,
          pearson_r: 0.91,
          rows: [],
        },
        {
          table_name: 'gsea.tsv',
          n_rows_parent: 40,
          n_rows_child: 42,
          n_overlap: 38,
          n_robust: 20,
          n_concordant: 14,
          n_discordant: 4,
          n_entity_missing: 0,
          n_numerics_incomplete: 0,
          rows: [],
        },
      ],
    }

    render(
      <SensitivityComparisonCard
        stage="compare_versions"
        candidates={[]}
        onSelect={vi.fn()}
        crossVersion={report}
      />,
    )

    // Version-mode heading + parent/child labels.
    expect(screen.getByRole('heading', { level: 3 }).textContent).toMatch(
      /Cross-version diff/i,
    )
    expect(screen.getByText('pkg-v1')).toBeInTheDocument()
    expect(screen.getByText('pkg-v2')).toBeInTheDocument()
    expect(screen.getByText(/87\.5%/)).toBeInTheDocument()

    // No method-picker affordances in version mode.
    expect(screen.queryByRole('radiogroup')).toBeNull()
    expect(
      screen.queryByRole('button', { name: /Record choice/i }),
    ).toBeNull()
    expect(screen.queryByLabelText(/Optional rationale/i)).toBeNull()

    // One progressbar per table.
    const bars = screen.getAllByRole('progressbar')
    expect(bars.length).toBe(2)

    // Region carries the discriminant for e2e hooks.
    const region = screen.getByRole('region')
    expect(region.getAttribute('data-mode')).toBe('versions')
  })
})
