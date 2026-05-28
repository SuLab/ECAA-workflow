import { describe, expect, it } from 'vitest'
import { parseIterTaskId, planIterCollapse } from './DagCanvas'
import type { DAG, Task } from '../types'

function task(depends: string[] = []): Task {
  return {
    kind: 'computation',
    state: { status: 'completed' },
    depends_on: depends,
    assignee: 'agent',
    description: '',
    spec: null,
    resolution: null,
    result_ref: null,
    resource_class: 'cpu_heavy',
    requires_sme_review: false,
    required_artifacts: [],
  } as unknown as Task
}

function buildDag(tasks: Record<string, Task>): DAG {
  return { tasks } as DAG
}

describe('parseIterTaskId', () => {
  it('parses canonical <base>_iter_<n> shape', () => {
    expect(parseIterTaskId('clustering_iter_3')).toEqual({
      base: 'clustering',
      n: 3,
    })
    expect(parseIterTaskId('a_b_c_iter_42')).toEqual({
      base: 'a_b_c',
      n: 42,
    })
  })

  it('rejects non-iter ids', () => {
    expect(parseIterTaskId('clustering')).toBeNull()
    expect(parseIterTaskId('clustering_iter')).toBeNull()
    expect(parseIterTaskId('clustering_iter_')).toBeNull()
    expect(parseIterTaskId('clustering_iter_x')).toBeNull()
    expect(parseIterTaskId('clustering_iter_0')).toBeNull()
    expect(parseIterTaskId('clustering_iter_-1')).toBeNull()
  })
})

describe('planIterCollapse', () => {
  it('does nothing when chain length is at or below the threshold', () => {
    const dag = buildDag({
      clustering_iter_1: task(),
      clustering_iter_2: task(['clustering_iter_1']),
      clustering_iter_3: task(['clustering_iter_2']),
      clustering_iter_4: task(['clustering_iter_3']),
      clustering_iter_5: task(['clustering_iter_4']),
    })
    const info = planIterCollapse(dag)
    expect(info.hiddenIds.size).toBe(0)
    expect(info.placeholders.size).toBe(0)
    expect(info.iterCounts.get('clustering')).toBe(5)
  })

  it('collapses chains that exceed the threshold (keeps iter_1 + last 2)', () => {
    const dag = buildDag({
      clustering_iter_1: task(),
      clustering_iter_2: task(['clustering_iter_1']),
      clustering_iter_3: task(['clustering_iter_2']),
      clustering_iter_4: task(['clustering_iter_3']),
      clustering_iter_5: task(['clustering_iter_4']),
      clustering_iter_6: task(['clustering_iter_5']),
      clustering_iter_7: task(['clustering_iter_6']),
      clustering_iter_8: task(['clustering_iter_7']),
    })
    const info = planIterCollapse(dag)
    // Hidden = iter_2..iter_6 (the middle 5).
    expect(info.hiddenIds.size).toBe(5)
    expect(info.hiddenIds.has('clustering_iter_2')).toBe(true)
    expect(info.hiddenIds.has('clustering_iter_6')).toBe(true)
    expect(info.hiddenIds.has('clustering_iter_1')).toBe(false)
    expect(info.hiddenIds.has('clustering_iter_7')).toBe(false)
    expect(info.hiddenIds.has('clustering_iter_8')).toBe(false)
    // One placeholder synthesized.
    const ph = info.placeholders.get('clustering_iter_collapsed')
    expect(ph).toBeDefined()
    expect(ph?.precedingIter).toBe('clustering_iter_1')
    expect(ph?.followingIter).toBe('clustering_iter_7')
    expect(ph?.hiddenIters).toEqual([2, 3, 4, 5, 6])
  })

  it('handles two independent iter chains independently', () => {
    const dag = buildDag({
      // Chain A — long (collapses).
      a_iter_1: task(),
      a_iter_2: task(['a_iter_1']),
      a_iter_3: task(['a_iter_2']),
      a_iter_4: task(['a_iter_3']),
      a_iter_5: task(['a_iter_4']),
      a_iter_6: task(['a_iter_5']),
      a_iter_7: task(['a_iter_6']),
      // Chain B — short (stays).
      b_iter_1: task(),
      b_iter_2: task(['b_iter_1']),
      b_iter_3: task(['b_iter_2']),
    })
    const info = planIterCollapse(dag)
    expect(info.placeholders.size).toBe(1)
    expect(info.placeholders.has('a_iter_collapsed')).toBe(true)
    expect(info.placeholders.has('b_iter_collapsed')).toBe(false)
    expect(info.iterCounts.get('a')).toBe(7)
    expect(info.iterCounts.get('b')).toBe(3)
  })

  it('sorts iter ids by numeric N regardless of insertion order', () => {
    // Same chain, ordered backward. Result must still pick the right
    // first/last/middle.
    const dag = buildDag({
      x_iter_8: task(),
      x_iter_7: task(),
      x_iter_6: task(),
      x_iter_5: task(),
      x_iter_4: task(),
      x_iter_3: task(),
      x_iter_2: task(),
      x_iter_1: task(),
    })
    const info = planIterCollapse(dag)
    const ph = info.placeholders.get('x_iter_collapsed')
    expect(ph?.precedingIter).toBe('x_iter_1')
    expect(ph?.followingIter).toBe('x_iter_7')
    expect(ph?.hiddenIters).toEqual([2, 3, 4, 5, 6])
  })

  it('captures iter counts even for chains under the collapse threshold', () => {
    // The TaskCard "iter ×N" badge reads from iterCounts regardless of
    // whether the chain visually collapses; verify the count is recorded
    // even when nothing is hidden.
    const dag = buildDag({
      tiny_iter_1: task(),
      tiny_iter_2: task(['tiny_iter_1']),
    })
    const info = planIterCollapse(dag)
    expect(info.iterCounts.get('tiny')).toBe(2)
    expect(info.hiddenIds.size).toBe(0)
  })

  it('ignores non-iter tasks completely', () => {
    const dag = buildDag({
      classify_intake: task(),
      iterate_gate_clustering: task(),
      iterate_check_clustering: task(),
    })
    const info = planIterCollapse(dag)
    expect(info.chains.size).toBe(0)
    expect(info.iterCounts.size).toBe(0)
    expect(info.hiddenIds.size).toBe(0)
  })
})
