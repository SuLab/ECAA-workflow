import { describe, expect, it, beforeEach, vi } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'

import type { AuditProofReport } from '../../types/AuditProofReport'
import type { ReplayReport } from '../../types/ReplayReport'
import {
  getAuditProof,
  getReplay,
  replayVerify,
  reverifyAuditProof,
  startReplayReproduce,
} from '../../api/chatClient'
import { ReproducibilityTab } from './ReproducibilityTab'

vi.mock('../../api/chatClient', () => ({
  getAuditProof: vi.fn(),
  getReplay: vi.fn(),
  replayVerify: vi.fn(),
  reverifyAuditProof: vi.fn(),
  startReplayReproduce: vi.fn(),
}))

const mockGetAuditProof = vi.mocked(getAuditProof)
const mockGetReplay = vi.mocked(getReplay)
const mockReplayVerify = vi.mocked(replayVerify)
const mockReverify = vi.mocked(reverifyAuditProof)
const mockStartReplayReproduce = vi.mocked(startReplayReproduce)

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
  // Default: no backgrounded replay job (the tab rehydrates one on mount).
  mockGetReplay.mockResolvedValue({ status: 'idle' })
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
    mockReplayVerify.mockResolvedValue(replay)

    render(<ReproducibilityTab sessionId="s1" />)
    await screen.findByText('claim_completeness')

    await userEvent.click(screen.getByTestId('integrity-button'))

    await waitFor(() =>
      expect(screen.getByTestId('integrity-verdict')).toHaveTextContent(/pass/i),
    )
    expect(mockReplayVerify).toHaveBeenCalledWith('s1')
  })

  it('polls the backgrounded full-reproduce job and explains an unprovisionable runtime', async () => {
    mockGetAuditProof.mockResolvedValue(fullReport())
    // POST …/replay {tier:'all'} returns 202 { replay_id } (no synchronous report).
    mockStartReplayReproduce.mockResolvedValue({ replay_id: 'r1' })
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
    // Mount rehydrate sees no job (idle); the poll after the click sees it done.
    mockGetReplay
      .mockResolvedValueOnce({ status: 'idle' })
      .mockResolvedValue({ status: 'done', report: doneReport })

    // Real timers: the tab polls getReplay on a 3s interval, so the waitFor
    // below is given a > 3s budget. Fake timers deadlock against vi.waitFor's
    // own polling, so we intentionally use wall-clock here.
    render(<ReproducibilityTab sessionId="s1" />)
    await screen.findByText('claim_completeness')

    await userEvent.click(screen.getByTestId('reproduce-button'))
    expect(mockStartReplayReproduce).toHaveBeenCalledWith('s1')
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

  it('shows the verifier-less note and disables Tier-2 reproduce for an imported non-re-executable package', async () => {
    mockGetAuditProof.mockResolvedValue(fullReport())
    const capabilities = {
      tier_label: 'minimal_audit' as const,
      explore: true,
      reverify: true,
      replay_tier1: true,
      replay_tier2: false,
      tabs: {} as Record<string, boolean>,
    }
    render(
      <ReproducibilityTab
        sessionId="s1"
        imported
        capabilities={capabilities}
      />,
    )
    await screen.findByText('claim_completeness')

    expect(screen.getByTestId('verifierless-note')).toBeInTheDocument()
    const reproduce = screen.getByTestId('reproduce-button')
    expect(reproduce).toBeDisabled()
    expect(reproduce).toHaveAttribute(
      'title',
      expect.stringMatching(/re-executable/i),
    )
  })

  it('omits the verifier-less note and enables Tier-2 for a re-executable imported package', async () => {
    mockGetAuditProof.mockResolvedValue(fullReport())
    const capabilities = {
      tier_label: 're_executable' as const,
      explore: true,
      reverify: true,
      replay_tier1: true,
      replay_tier2: true,
      tabs: {} as Record<string, boolean>,
    }
    render(
      <ReproducibilityTab
        sessionId="s1"
        imported
        capabilities={capabilities}
      />,
    )
    await screen.findByText('claim_completeness')

    // Note still renders (imported), but Tier-2 is enabled.
    expect(screen.getByTestId('verifierless-note')).toBeInTheDocument()
    expect(screen.getByTestId('reproduce-button')).not.toBeDisabled()
  })
})
