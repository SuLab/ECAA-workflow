// Vitest tests for AffordanceBadge.
// Covers: each affordance variant renders the right text + aria-label;
// axe a11y audit on all five variants.

import { describe, expect, it } from 'vitest'
import { render } from '@testing-library/react'
import axe from 'axe-core'
import { AffordanceBadge } from './AffordanceBadge'
import type { PlotAffordance } from '../types/PlotAffordance'

// ── Fixture helpers ────────────────────────────────────────────────────────

const proof = {
  source_semantic_type: 'ecaax:de_output',
  ontology_walk: ['EDAM:data_3134'],
  registry_snapshot_id: 'snap-v1',
  theme_version: '1.0',
  rationale: 'Test rationale',
}

const registered: PlotAffordance = {
  kind: 'registered',
  figure_ids: ['volcano'],
  renderer_module: 'stages.de',
  proof,
}

const inherited: PlotAffordance = {
  kind: 'inherited_via_ontology',
  parent_term: 'EDAM:data_3134',
  figure_ids: ['scatter'],
  renderer_module: 'stages.generic_scatter',
  proof,
}

const structural: PlotAffordance = {
  kind: 'structural_fallback',
  primitive: 'matrix_overview',
  figure_id: '__structural_matrix_overview',
  warning: 'No registered renderer; using structural fallback.',
  proof,
}

const generated: PlotAffordance = {
  kind: 'generated_sandboxed',
  renderer_module: 'generated.abc123',
  figure_ids: ['my_violin'],
  review_status: 'drafted',
  proof,
}

const deferred: PlotAffordance = {
  kind: 'deferred',
  data_artifact_relpath: 'runtime/outputs/de/result.h5ad',
  recommendation: 'Add required_figures to the atom.',
  sme_check_required: true,
  proof,
}

// ── Text + aria-label assertions ──────────────────────────────────────────

describe('AffordanceBadge', () => {
  it('registered: renders "Validated" with aria-label', () => {
    const { getByText, container } = render(<AffordanceBadge affordance={registered} />)
    expect(getByText('Validated')).toBeTruthy()
    const span = container.querySelector('span')
    expect(span?.getAttribute('aria-label')).toBe('Validated renderer')
  })

  it('inherited_via_ontology: renders parent_term text + aria-label + title tooltip', () => {
    const { container } = render(<AffordanceBadge affordance={inherited} />)
    const span = container.querySelector('span')
    expect(span?.textContent).toContain('EDAM:data_3134')
    expect(span?.getAttribute('aria-label')).toContain('EDAM:data_3134')
    expect(span?.getAttribute('title')).toBe(proof.rationale)
  })

  it('structural_fallback: renders primitive in label text + title = warning', () => {
    const { container } = render(<AffordanceBadge affordance={structural} />)
    const span = container.querySelector('span')
    expect(span?.textContent).toContain('matrix_overview')
    expect(span?.getAttribute('title')).toBe('No registered renderer; using structural fallback.')
    expect(span?.getAttribute('aria-label')).toContain('matrix_overview')
  })

  it('generated_sandboxed: renders review_status in label + aria-label', () => {
    const { container } = render(<AffordanceBadge affordance={generated} />)
    const span = container.querySelector('span')
    expect(span?.textContent).toContain('drafted')
    expect(span?.getAttribute('aria-label')).toContain('drafted')
  })

  it('deferred: renders "No automatic plot" text + aria-label', () => {
    const { getByText, container } = render(<AffordanceBadge affordance={deferred} />)
    expect(getByText(/No automatic plot/i)).toBeTruthy()
    const span = container.querySelector('span')
    expect(span?.getAttribute('aria-label')).toBe('No automatic plot')
  })
})

// ── axe a11y ──────────────────────────────────────────────────────────────

async function runAxe(node: HTMLElement) {
  return axe.run(node, {
    runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'] },
    rules: { 'color-contrast': { enabled: false } },
  })
}

describe('AffordanceBadge a11y (axe-core)', () => {
  it('registered has no axe violations', async () => {
    const { container } = render(<AffordanceBadge affordance={registered} />)
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })

  it('inherited_via_ontology has no axe violations', async () => {
    const { container } = render(<AffordanceBadge affordance={inherited} />)
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })

  it('structural_fallback has no axe violations', async () => {
    const { container } = render(<AffordanceBadge affordance={structural} />)
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })

  it('generated_sandboxed has no axe violations', async () => {
    const { container } = render(<AffordanceBadge affordance={generated} />)
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })

  it('deferred has no axe violations', async () => {
    const { container } = render(<AffordanceBadge affordance={deferred} />)
    const results = await runAxe(container)
    expect(results.violations).toEqual([])
  })
})
