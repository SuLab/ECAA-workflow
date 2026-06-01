import { render, screen, fireEvent } from '@testing-library/react'
import { describe, it, expect, vi } from 'vitest'
import { MethodOptionsCard } from './MethodOptionsCard'

describe('MethodOptionsCard', () => {
  it('renders ranked options with evidence and records a choice', async () => {
    const onSelect = vi.fn()
    render(
      <MethodOptionsCard
        stage="discover_alignment"
        options={[
          {
            method: 'star',
            score: 4.2,
            literatureEligible: true,
            evidence: [
              {
                sourceClass: 'primary_literature',
                ref: 'PMID:30000000',
                quote: 'STAR aligns…',
              },
            ],
          },
          {
            method: 'hisat2',
            score: 3.1,
            literatureEligible: false,
            evidence: [
              {
                sourceClass: 'tool_documentation',
                ref: 'https://rtd/x',
                quote: 'supports…',
              },
            ],
          },
        ]}
        onSelect={onSelect}
      />,
    )
    expect(screen.getByText(/star/)).toBeInTheDocument()
    // rank-1 default is marked recommended.
    expect(screen.getByText(/★ Recommended/)).toBeInTheDocument()
    fireEvent.click(screen.getByLabelText(/hisat2/))
    fireEvent.click(screen.getByRole('button', { name: /record choice/i }))
    expect(onSelect).toHaveBeenCalledWith('hisat2', undefined)
  })

  it('renders the evidence list, literature flag, and tentative badge', () => {
    render(
      <MethodOptionsCard
        stage="discover_alignment"
        options={[
          {
            method: 'flair',
            score: 2.0,
            literatureEligible: false,
            tentative: true,
            evidence: [
              {
                sourceClass: 'conference_proceedings',
                ref: 'DOI:10.1/x',
                quote: 'flair handles…',
                versionContext: '1.5.0',
              },
            ],
          },
        ]}
        onSelect={vi.fn()}
      />,
    )
    expect(screen.getByText(/flair handles…/)).toBeInTheDocument()
    expect(screen.getByText(/DOI:10.1\/x/)).toBeInTheDocument()
    expect(screen.getByText(/tentative/)).toBeInTheDocument()
    expect(screen.getByText(/no paper-class evidence/)).toBeInTheDocument()
  })

  it('passes a typed rationale through to onSelect', () => {
    const onSelect = vi.fn()
    render(
      <MethodOptionsCard
        stage="discover_normalization"
        options={[
          {
            method: 'tmm',
            score: 5.0,
            literatureEligible: true,
            evidence: [],
          },
        ]}
        onSelect={onSelect}
      />,
    )
    fireEvent.change(screen.getByPlaceholderText(/why this choice/i), {
      target: { value: 'house standard' },
    })
    fireEvent.click(screen.getByRole('button', { name: /record choice/i }))
    expect(onSelect).toHaveBeenCalledWith('tmm', 'house standard')
  })
})
