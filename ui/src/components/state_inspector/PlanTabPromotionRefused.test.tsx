// V3+v4 residuals closure PlanTab PromotionRefused mount test.
//
// Confirms the PromotionRefusedCard surfaces above the DagCanvas when
// `/compose-outcome` returns a refusal whose typed kind is
// `promotion_refused`.

import { render, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

// Mock the DagCanvas + drawer surfaces so we don't drag the ThemeProvider
// into the render tree just to validate the refusal card mount.
vi.mock('../DagCanvas', () => ({
  default: () => null,
}))
vi.mock('../TaskDetailDrawer', () => ({
  default: () => null,
}))
vi.mock('../EdgeProofDrawer', () => ({
  default: () => null,
}))
vi.mock('../ProgressBar', () => ({
  default: () => null,
}))
vi.mock('../DagFilterChips', () => ({
  default: () => null,
  loadFilter: () => new Set<string>(),
}))

import * as chatClient from '../../api/chatClient'
import { PlanTab } from './PlanTab'
import type { DAG } from '../../types'

const fakeDag = {
  tasks: {},
  edges: [],
} as unknown as DAG

beforeEach(() => {
  // Stub the proofs endpoint so the DagCanvas mount doesn't trip on
  // missing data.
  vi.spyOn(chatClient, 'getProofs').mockResolvedValue({ proofs: [] })
})

afterEach(() => {
  vi.restoreAllMocks()
})

describe('PlanTab — PromotionRefused mount', () => {
  it('renders PromotionRefusedCard when compose outcome is a promotion refusal', async () => {
    // `ComposeOutcomePayload.refusal` is typed loosely (`kind?: string`)
    // in the chat client because the server serializes the full
    // `RefusalReport` JSON; the PlanTab effect runtime-narrows on
    // `refusal.kind.kind`. Cast to `any` here so the test fixture can
    // mirror the actual wire shape without fighting the public type.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    vi.spyOn(chatClient, 'getComposeOutcome').mockResolvedValue({
      variant: 'refusal',
      summary: 'Promotion refused',
      node_count: 0,
      edge_count: 0,
      assumption_count: 0,
      accepted_nodes: [],
      refusal: {
        id: 'promotion_refused',
        kind: { kind: 'promotion_refused' },
        statement: '1 node failed the validation × lifecycle promotion grid',
        references: ['node_blocked'],
        unblock_paths: [
          {
            kind: 'escalate_to_reviewer',
            reviewer_class: 'bioinformatics_lead',
            required_artifacts: [],
            target_outcome: 'draft_dag',
          },
        ],
      },
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
    } as any)
    const { findByRole } = render(
      <PlanTab dag={fakeDag} sessionId="s1" parentSessionId={null} />,
    )
    await waitFor(async () => {
      expect(await findByRole('region', { name: /Promotion refused/i })).toBeInTheDocument()
    })
  })

  it('does NOT render PromotionRefusedCard when compose outcome is a different refusal', async () => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    vi.spyOn(chatClient, 'getComposeOutcome').mockResolvedValue({
      variant: 'refusal',
      summary: 'License missing',
      node_count: 0,
      edge_count: 0,
      assumption_count: 0,
      accepted_nodes: [],
      refusal: {
        id: 'license_missing',
        kind: { kind: 'license_missing' },
        statement: 'License missing',
        references: [],
        unblock_paths: [],
      },
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
    } as any)
    const { queryByRole } = render(
      <PlanTab dag={fakeDag} sessionId="s1" parentSessionId={null} />,
    )
    // Drain microtasks so the effect's fetch resolves before asserting
    // the negative-render.
    await new Promise((resolve) => setTimeout(resolve, 10))
    expect(queryByRole('region', { name: /Promotion refused/i })).toBeNull()
  })
})
