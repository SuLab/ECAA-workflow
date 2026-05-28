import { describe, expect, it } from 'vitest'
import { render } from '@testing-library/react'
import { ReactFlowProvider } from '@xyflow/react'
import TaskCard, { type TaskCardData } from './TaskCard'
import type { Task } from '../types'
import type { SafetyPolicy } from '../types/SafetyPolicy'

const SAFETY_COMPUTE: SafetyPolicy = {
  level: 'compute',
  network: { kind: 'none', allowlist: [] },
  code_execution: 'none',
  sandbox: 'none',
  provisioning: 'declared_only',
  controlled_access: false,
}

function task(overrides: Partial<Task> = {}): Task {
  return {
    kind: 'computation',
    state: { status: 'completed' },
    depends_on: [],
    assignee: 'agent',
    description: 'demo task',
    spec: null,
    resolution: null,
    result_ref: null,
    resource_class: 'cpu_heavy',
    requires_sme_review: false,
    required_artifacts: [],
    ...overrides,
  } as unknown as Task
}

function renderCard(id: string, data: TaskCardData) {
  // ReactFlow nodes only render correctly inside a ReactFlowProvider —
  // the Handle component reads context for edge endpoints. Wrap the
  // single-card render in the provider so the handles don't throw.
  // ReactFlow's NodeProps requires several layout fields the runtime
  // synthesises on its own; for unit tests we hand-roll a minimal
  // shape and cast through `as never` so the strict NodeProps doesn't
  // reject the construction here.
  // The cast goes through `unknown` first because ReactFlow's
  // NodeProps signature is intentionally strict; the test only relies
  // on the three fields TaskCard reads from props.
  const Card = TaskCard as unknown as (
    p: { id: string; type: string; data: TaskCardData },
  ) => JSX.Element
  return render(
    <ReactFlowProvider>
      <Card id={id} type="taskCard" data={data} />
    </ReactFlowProvider>,
  )
}

describe('TaskCard — Plan §S10.8 iterate-until role surfacing', () => {
  it('renders the iterate-role badge when spec carries iterate_role', () => {
    const t = task({
      spec: { iterate_role: 'check', cardinality: 'iterate_until' },
    })
    const { container } = renderCard('iterate_check_clustering', { task: t })
    const badge = container.querySelector('[data-iter-role-badge="check"]')
    expect(badge).not.toBeNull()
    expect(badge?.textContent).toMatch(/check/i)
  })

  it('renders "iter" label for placeholder role (the canonical iter task)', () => {
    const t = task({
      spec: { iterate_role: 'placeholder', cardinality: 'iterate_until' },
    })
    const { container } = renderCard('clustering', { task: t, iterCount: 7 })
    const badge = container.querySelector('[data-iter-role-badge="placeholder"]')
    expect(badge?.textContent).toMatch(/iter/i)
    const count = container.querySelector('[data-iter-count="7"]')
    expect(count?.textContent).toContain('×7')
  })

  it('renders nothing on tasks without an iterate_role spec', () => {
    const t = task({ spec: { stage_class: 'clustering' } })
    const { container } = renderCard('clustering', { task: t })
    expect(container.querySelector('[data-iter-role-badge]')).toBeNull()
    expect(container.querySelector('[data-iter-count]')).toBeNull()
  })

  it('renders the synthetic +N more placeholder with dashed border + collapsed label', () => {
    const t = task({
      spec: { iterate_role: 'collapsed_segment', base: 'clustering' },
    })
    const { container } = renderCard('clustering_iter_collapsed', {
      task: t,
      collapsedChain: { base: 'clustering', hiddenCount: 5 },
    })
    expect(
      container.querySelector('[data-collapsed-chain="true"]'),
    ).not.toBeNull()
    expect(container.textContent).toContain('+5 more iterations')
    expect(container.textContent).toContain('clustering chain')
    // The role badge is suppressed on the collapsed placeholder so the
    // "+N more" label is the sole headline.
    expect(container.querySelector('[data-iter-role-badge]')).toBeNull()
  })

  it('omits the iter-count chip when count is zero or missing', () => {
    const t = task({ spec: { iterate_role: 'gate' } })
    const { container } = renderCard('iterate_gate_clustering', {
      task: t,
      iterCount: 0,
    })
    expect(container.querySelector('[data-iter-count]')).toBeNull()
  })
})

describe('TaskCard — Phase 8.2 SafetyBadge mount', () => {
  it('renders the SafetyBadge when task.safety is present', () => {
    const t = task({ safety: SAFETY_COMPUTE })
    const { container } = renderCard('compute_task', { task: t })
    const badge = container.querySelector('[data-safety-level="compute"]')
    expect(badge).not.toBeNull()
    expect(badge?.getAttribute('aria-label')).toMatch(/safety: compute/i)
  })

  it('suppresses the SafetyBadge on the synthetic +N collapsed-chain placeholder', () => {
    const t = task({
      safety: SAFETY_COMPUTE,
      spec: { iterate_role: 'collapsed_segment', base: 'clustering' },
    })
    const { container } = renderCard('clustering_iter_collapsed', {
      task: t,
      collapsedChain: { base: 'clustering', hiddenCount: 5 },
    })
    expect(container.querySelector('[data-safety-level]')).toBeNull()
  })
})
