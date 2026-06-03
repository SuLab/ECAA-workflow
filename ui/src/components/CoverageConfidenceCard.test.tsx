import { describe, expect, it, vi } from 'vitest'
import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import axe from 'axe-core'
import CoverageConfidenceCard from './CoverageConfidenceCard'
import type { CoverageConfidence } from '../types/CoverageConfidence'

function runAxe(node: Element) {
  return axe.run(node, {
    runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'] },
    rules: { 'color-contrast': { enabled: false } },
  })
}

const partial: CoverageConfidence = {
  fully_covered: false,
  partially_covered_modalities: [],
  uncovered_modalities: ['cytof'],
  gap_count: 1,
}

const full: CoverageConfidence = {
  fully_covered: true,
  partially_covered_modalities: [],
  uncovered_modalities: [],
  gap_count: 0,
}

describe('CoverageConfidenceCard', () => {
  it('renders uncovered modality + propose affordance', async () => {
    const { container } = render(
      <CoverageConfidenceCard coverage={partial} onProposeDraft={vi.fn()} />,
    )
    expect(screen.getByText(/outside our validated catalog/i)).toBeInTheDocument()
    expect(screen.getByText('cytof')).toBeInTheDocument()
    expect(
      screen.getByRole('button', { name: /Draft a candidate step/i }),
    ).toBeInTheDocument()
    const results = await runAxe(container)
    expect(results.violations).toHaveLength(0)
  })

  it('fires onProposeDraft when the CTA is clicked', async () => {
    const onProposeDraft = vi.fn()
    const user = userEvent.setup()
    render(<CoverageConfidenceCard coverage={partial} onProposeDraft={onProposeDraft} />)
    await user.click(screen.getByRole('button', { name: /Draft a candidate step/i }))
    expect(onProposeDraft).toHaveBeenCalledTimes(1)
  })

  it('renders nothing when fully covered', () => {
    const { container } = render(
      <CoverageConfidenceCard coverage={full} onProposeDraft={vi.fn()} />,
    )
    expect(container).toBeEmptyDOMElement()
  })
})
