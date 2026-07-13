import { afterEach, describe, expect, it, vi } from 'vitest'
import { getMarkdownIndex, normalizeDiscoveryDecision } from './chatClient'

describe('getMarkdownIndex', () => {
  afterEach(() => vi.restoreAllMocks())
  it('returns the docs array from the markdown-index endpoint', async () => {
    (globalThis as unknown as { fetch: typeof fetch }).fetch = vi.fn(
      async () =>
        new Response(
          JSON.stringify({
            docs: [
              { path: 'README.md', name: 'README.md', group: 'package' },
              {
                path: 'runtime/outputs/reporting/report.md',
                name: 'report.md',
                group: 'reporting',
              },
            ],
          }),
          { status: 200, headers: { 'Content-Type': 'application/json' } },
        ),
    ) as unknown as typeof fetch
    const docs = await getMarkdownIndex('s1')
    expect(docs).toHaveLength(2)
    expect(docs[0]!.name).toBe('README.md')
    expect(docs[1]!.group).toBe('reporting')
  })
})

describe('normalizeDiscoveryDecision', () => {
  it('maps the rich { chosen, candidate_pool_full } schema onto the legacy fields', () => {
    const raw = {
      task_id: 'discover_sequence_trimming',
      chosen: 'fastp',
      candidate_pool_full: [
        { method_id: 'fastp', rank: 1, composite_score: 4.8, rationale: 'best' },
        { method_id: 'trim_galore', rank: 2, composite_score: 4.5 },
        { method_id: 'cutadapt', rank: 3, composite_score: 4.2 },
        { method_id: 'trimmomatic', rank: 4, composite_score: 3.9 },
      ],
    }
    const d = normalizeDiscoveryDecision(raw)!
    expect(d.top_candidate).toBe('fastp')
    expect(d.runner_ups).toEqual(['trim_galore', 'cutadapt', 'trimmomatic'])
    // composite_score is on the 0–5 server scale; normalized to 0–1.
    expect(d.scores!['fastp']).toBeCloseTo(0.96)
    expect(d.scores!['trimmomatic']).toBeCloseTo(0.78)
    expect(d.rationale).toBe('best')
    expect(d.task_id).toBe('discover_sequence_trimming')
  })

  it('rank-orders runner-ups even when candidate_pool_full is unsorted', () => {
    const raw = {
      task_id: 't',
      chosen: 'a',
      candidate_pool_full: [
        { method_id: 'c', rank: 3, composite_score: 2 },
        { method_id: 'a', rank: 1, composite_score: 5 },
        { method_id: 'b', rank: 2, composite_score: 4 },
      ],
    }
    const d = normalizeDiscoveryDecision(raw)!
    expect(d.top_candidate).toBe('a')
    expect(d.runner_ups).toEqual(['b', 'c'])
  })

  it('falls back to rank-1 when chosen is absent', () => {
    const raw = {
      task_id: 't',
      candidate_pool_full: [
        { method_id: 'x', rank: 1, composite_score: 5 },
        { method_id: 'y', rank: 2, composite_score: 4 },
      ],
    }
    const d = normalizeDiscoveryDecision(raw)!
    expect(d.top_candidate).toBe('x')
    expect(d.runner_ups).toEqual(['y'])
  })

  it('passes a legacy top_candidate decision through unchanged', () => {
    const raw = {
      task_id: 'discover_normalization',
      top_candidate: 'vst',
      runner_ups: ['tmm', 'cpm'],
      scores: { vst: 0.91, tmm: 0.82, cpm: 0.71 },
      rationale: 'scorer pick',
      auto_picked: false,
    }
    expect(normalizeDiscoveryDecision(raw)).toEqual(raw)
  })

  it('returns null for null / non-object input', () => {
    expect(normalizeDiscoveryDecision(null)).toBeNull()
    expect(normalizeDiscoveryDecision(undefined)).toBeNull()
    expect(normalizeDiscoveryDecision('nope')).toBeNull()
  })
})
