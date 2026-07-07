import { describe, expect, it, beforeEach, vi } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'

import type { AuditProofReport } from '../../types/AuditProofReport'
import type { ReplayReport } from '../../types/ReplayReport'
import {
  getAuditProof,
  getReplay,
  reverifyAuditProof,
  startReplay,
} from '../../api/chatClient'
import { ReproducibilityTab } from './ReproducibilityTab'

vi.mock('../../api/chatClient', () => ({
  getAuditProof: vi.fn(),
  getReplay: vi.fn(),
  reverifyAuditProof: vi.fn(),
  startReplay: vi.fn(),
}))

const mockGetAuditProof = vi.mocked(getAuditProof)
const mockGetReplay = vi.mocked(getReplay)
const mockReverify = vi.mocked(reverifyAuditProof)
const mockStartReplay = vi.mocked(startReplay)

const INVARIANT_IDS = [
  'claim_completeness',
  'decision_justification',
  'evidence_coverage',
  'equivalence_failure',
  'cross_graph_integrity',
  'substrate_validity',
] as const

function fullReport(): AuditProofReport {
  return {
    schema_version: '0.2',
    ecaa_version: '0.2',
    min_reader_version: '0.2',
    evaluator: { impl: 'ecaa', version: '1', policy: 'warn-only' },
    verdicts: INVARIANT_IDS.map((id) => ({
      id,
      status: 'pass',
      detail: null,
      n_inspected: 3,
      n_violations: 0,
    })),
  } as AuditProofReport
}

beforeEach(() => {
  vi.clearAllMocks()
})

describe('ReproducibilityTab', () => {
  it('renders the six audit-proof invariants from the loaded report', async () => {
    mockGetAuditProof.mockResolvedValue(fullReport())
    render(<ReproducibilityTab sessionId="s1" />)

    for (const id of INVARIANT_IDS) {
      expect(await screen.findByText(id)).toBeInTheDocument()
    }
    expect(mockGetAuditProof).toHaveBeenCalledWith('s1')
  })

  it('shows the empty state when no report exists yet', async () => {
    mockGetAuditProof.mockResolvedValue(null)
    render(<ReproducibilityTab sessionId="s1" />)

    expect(
      await screen.findByText(/no audit-proof report yet/i),
    ).toBeInTheDocument()
  })

  it('re-verifies on button click and re-renders the fresh report', async () => {
    mockGetAuditProof.mockResolvedValue(null)
    const fresh = fullReport()
    fresh.verdicts[0]!.status = 'fail'
    fresh.verdicts[0]!.n_violations = 2
    mockReverify.mockResolvedValue(fresh)

    render(<ReproducibilityTab sessionId="s1" />)
    await screen.findByText(/no audit-proof report yet/i)

    await userEvent.click(screen.getByTestId('reverify-button'))

    await waitFor(() =>
      expect(screen.getByText('claim_completeness')).toBeInTheDocument(),
    )
    expect(mockReverify).toHaveBeenCalledWith('s1')
    // The failing invariant's violation count is surfaced.
    expect(screen.getByTestId('invariant-row-claim_completeness')).toHaveTextContent(
      '2',
    )
  })

  it('runs the Tier-1 integrity check and shows its verdict', async () => {
    mockGetAuditProof.mockResolvedValue(fullReport())
    const replay: ReplayReport = {
      schema_version: '0.2',
      package_iri: 'pkg',
      reader_version: '1',
      min_reader_version: null,
      reverify: null,
      reexecute: null,
      skipped: [],
      verdict: 'pass',
    }
    mockStartReplay.mockResolvedValue(replay)

    render(<ReproducibilityTab sessionId="s1" />)
    await screen.findByText('claim_completeness')

    await userEvent.click(screen.getByTestId('integrity-button'))

    await waitFor(() =>
      expect(screen.getByTestId('integrity-verdict')).toHaveTextContent(/pass/i),
    )
    expect(mockStartReplay).toHaveBeenCalledWith('s1', { tier: 'verify' })
  })

  it('polls the backgrounded full-reproduce job and explains an unprovisionable runtime', async () => {
    mockGetAuditProof.mockResolvedValue(fullReport())
    // POST …/replay {tier:'all'} returns 202 (no synchronous body used).
    mockStartReplay.mockResolvedValue({} as ReplayReport)
    const doneReport: ReplayReport = {
      schema_version: '0.2',
      package_iri: 'pkg',
      reader_version: '1',
      min_reader_version: null,
      reverify: null,
      reexecute: { env_tier: 'local', report: {} as never, unprovisionable: true },
      skipped: [],
      verdict: 'partial',
    }
    mockGetReplay.mockResolvedValue({ status: 'done', report: doneReport })

    // Real timers: the tab polls getReplay on a 3s interval, so the
    // waitFor below is given a > 3s budget. Fake timers deadlock against
    // vi.waitFor's own polling, so we intentionally use wall-clock here.
    render(<ReproducibilityTab sessionId="s1" />)
    await screen.findByText('claim_completeness')

    await userEvent.click(screen.getByTestId('reproduce-button'))
    expect(mockStartReplay).toHaveBeenCalledWith('s1', { tier: 'all' })
    // Button flips to the in-flight label immediately.
    expect(screen.getByTestId('reproduce-button')).toHaveTextContent(/reproducing/i)

    // Poll fires at 3s → terminal report → unprovisionable explainer.
    await waitFor(
      () =>
        expect(screen.getByTestId('unprovisionable-explainer')).toBeInTheDocument(),
      { timeout: 5000 },
    )
    expect(screen.getByTestId('reproduce-result')).toHaveTextContent(/partial/i)
  }, 10000)

  it('renders a no-session placeholder', () => {
    render(<ReproducibilityTab sessionId={null} />)
    expect(screen.getByText(/no session selected/i)).toBeInTheDocument()
  })
})
