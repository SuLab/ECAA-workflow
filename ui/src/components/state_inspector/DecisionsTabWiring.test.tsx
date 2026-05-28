// V3+v4 residuals closure DecisionsTab wiring tests for the
// LifecycleAdjudicationCard + GraduationCandidateCard mounts.
//
// Verifies that:
// - The adjudication card mounts when `/adjudication` returns ≥ 1 entry.
// - The graduation card mounts when `/graduation/candidates` returns ≥ 1.
// - Both render under the Decisions tab body alongside the decision list.

import { render, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import * as chatClient from '../../api/chatClient'
import { DecisionsTab } from './DecisionsTab'
import type { AdjudicationQueueEntry } from '../../types/AdjudicationQueueEntry'

const sampleAdjudication: AdjudicationQueueEntry = {
  id: 'adj_aaaaaa',
  created_at: '2026-05-11T12:00:00Z',
  transition: {
    kind: 'same_user_contradiction',
    actor: 'alan',
    assumption_id: 'assumption_1',
    prior_record_id: 'rec_1',
    new_record_id: 'rec_2',
  },
  status: { kind: 'open' },
}

const sampleGraduation = {
  thresholds: {
    min_usage_count: 3,
    min_unique_sessions: 2,
    min_success_rate: 0.5,
  },
  candidates: [
    {
      iri: 'local:custom_qc_step',
      label: 'Custom QC step',
      usage_count: 5,
      unique_sessions: 3,
      success_rate: 0.85,
      graduation_target_ontology: 'EDAM',
    },
  ],
}

beforeEach(() => {
  vi.spyOn(chatClient, 'getDecisions').mockResolvedValue({
    session_id: 's1',
    decisions: [],
  })
})

afterEach(() => {
  vi.restoreAllMocks()
})

describe('DecisionsTab v3/v4 wiring', () => {
  it('mounts LifecycleAdjudicationCard when /adjudication returns ≥ 1 entry', async () => {
    vi.spyOn(chatClient, 'fetchAdjudicationQueue').mockResolvedValue([
      sampleAdjudication,
    ])
    vi.spyOn(chatClient, 'fetchGraduationCandidates').mockResolvedValue({
      thresholds: sampleGraduation.thresholds,
      candidates: [],
    })
    const { findByLabelText } = render(<DecisionsTab sessionId="s1" />)
    await waitFor(async () => {
      expect(
        await findByLabelText(/Lifecycle adjudication queue/i),
      ).toBeInTheDocument()
    })
  })

  it('mounts GraduationCandidateCard when /graduation/candidates returns ≥ 1', async () => {
    vi.spyOn(chatClient, 'fetchAdjudicationQueue').mockResolvedValue([])
    vi.spyOn(chatClient, 'fetchGraduationCandidates').mockResolvedValue(
      sampleGraduation,
    )
    const { findByLabelText } = render(<DecisionsTab sessionId="s1" />)
    await waitFor(async () => {
      expect(
        await findByLabelText(/graduation candidates/i),
      ).toBeInTheDocument()
    })
  })

  it('mounts neither card when both endpoints are empty', async () => {
    vi.spyOn(chatClient, 'fetchAdjudicationQueue').mockResolvedValue([])
    vi.spyOn(chatClient, 'fetchGraduationCandidates').mockResolvedValue({
      thresholds: sampleGraduation.thresholds,
      candidates: [],
    })
    const { queryByLabelText, findByText } = render(
      <DecisionsTab sessionId="s1" />,
    )
    // Wait for the empty-state decision message to confirm the tab
    // rendered without either card.
    await findByText(/No decisions recorded yet/i)
    expect(queryByLabelText(/Lifecycle adjudication queue/i)).toBeNull()
    expect(queryByLabelText(/graduation candidates/i)).toBeNull()
  })
})
