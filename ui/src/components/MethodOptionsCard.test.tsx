import { render, screen, fireEvent } from '@testing-library/react'
import { describe, it, expect, vi } from 'vitest'
import {
  MethodOptionsCard,
  axisFromStageId,
  mapLandscapeToOptions,
} from './MethodOptionsCard'
import type { MethodLandscape } from '../api/chatClient'

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

// A method_landscape.json payload conforming exactly to the artifact
// shape the survey_method_landscape agent emits.
const LANDSCAPE: MethodLandscape = {
  schema_version: 1,
  axes: {
    alignment: {
      candidates: [
        {
          method: 'hisat2',
          literature_eligible: false,
          tentative: false,
          support_score: 3.1,
          evidence: [
            {
              source_class: 'tool_documentation',
              source_ref_kind: 'url',
              source_ref: 'https://rtd/x',
              evidence_quote: 'HISAT2 supports spliced alignment',
              version_context: null,
            },
          ],
        },
        {
          method: 'star',
          literature_eligible: true,
          tentative: false,
          support_score: 4.7,
          evidence: [
            {
              source_class: 'primary_literature',
              source_ref_kind: 'pmid',
              source_ref: '30000000',
              evidence_quote: 'STAR aligns RNA-seq reads to a genome',
              version_context: '2.7.10',
            },
          ],
        },
        {
          method: 'flair',
          literature_eligible: false,
          tentative: true,
          support_score: 2.0,
          evidence: [
            {
              source_class: 'conference_proceedings',
              source_ref_kind: 'doi',
              source_ref: '10.1/abc',
              evidence_quote: 'FLAIR collapses long-read isoforms',
              version_context: null,
            },
          ],
        },
      ],
    },
  },
}

describe('axisFromStageId', () => {
  it('strips the discover_ prefix to get the axis key', () => {
    expect(axisFromStageId('discover_alignment')).toBe('alignment')
    expect(axisFromStageId('discover_normalization')).toBe('normalization')
  })
  it('leaves a non-discover id unchanged', () => {
    expect(axisFromStageId('alignment')).toBe('alignment')
  })
})

describe('mapLandscapeToOptions', () => {
  it('maps the artifact axis to ranked MethodOption[] with real evidence + scores', () => {
    const opts = mapLandscapeToOptions(LANDSCAPE, 'alignment')
    expect(opts).not.toBeNull()
    const ranked = opts!
    // Sorted by descending support_score: star (4.7) > hisat2 (3.1) > flair (2.0).
    expect(ranked.map((o) => o.method)).toEqual(['star', 'hisat2', 'flair'])
    const star = ranked[0]!
    expect(star.score).toBe(4.7)
    expect(star.literatureEligible).toBe(true)
    expect(star.tentative).toBe(false)
    expect(star.evidence).toEqual([
      {
        sourceClass: 'primary_literature',
        ref: 'pmid:30000000',
        quote: 'STAR aligns RNA-seq reads to a genome',
        versionContext: '2.7.10',
      },
    ])
    // tentative + literature flags propagate through.
    const flair = ranked.find((o) => o.method === 'flair')!
    expect(flair.tentative).toBe(true)
    expect(flair.literatureEligible).toBe(false)
    const flairEvidence = flair.evidence[0]!
    expect(flairEvidence.ref).toBe('doi:10.1/abc')
    expect(flairEvidence.versionContext).toBeUndefined()
  })

  it('returns null for a missing artifact or absent axis (placeholder fallback)', () => {
    expect(mapLandscapeToOptions(null, 'alignment')).toBeNull()
    expect(mapLandscapeToOptions(undefined, 'alignment')).toBeNull()
    expect(mapLandscapeToOptions(LANDSCAPE, 'normalization')).toBeNull()
    expect(
      mapLandscapeToOptions(
        { schema_version: 1, axes: { alignment: { candidates: [] } } },
        'alignment',
      ),
    ).toBeNull()
  })
})

describe('MethodOptionsCard with mapped landscape data', () => {
  it('renders real literature evidence + scores from a method_landscape.json payload', () => {
    const onSelect = vi.fn()
    const opts = mapLandscapeToOptions(LANDSCAPE, 'alignment')!
    render(
      <MethodOptionsCard
        stage="discover_alignment"
        options={opts}
        onSelect={onSelect}
      />,
    )
    // rank-1 (highest support_score) is the recommended default.
    expect(screen.getByText(/★ Recommended/)).toBeInTheDocument()
    expect(screen.getByLabelText(/^star$/)).toBeChecked()
    // Real score from the artifact, not a placeholder ordinal.
    expect(screen.getByText(/score 4\.70/)).toBeInTheDocument()
    // Real locator-anchored evidence quote + ref.
    expect(
      screen.getByText(/STAR aligns RNA-seq reads to a genome/),
    ).toBeInTheDocument()
    expect(screen.getByText(/pmid:30000000/)).toBeInTheDocument()
    // tentative + no-paper-class badges from the artifact flags.
    expect(screen.getByText(/tentative/)).toBeInTheDocument()
    // Selecting a non-default candidate records that choice.
    fireEvent.click(screen.getByLabelText(/^hisat2$/))
    fireEvent.click(screen.getByRole('button', { name: /record choice/i }))
    expect(onSelect).toHaveBeenCalledWith('hisat2', undefined)
  })
})
