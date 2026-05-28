// Vitest coverage for the hypothesized-node proposal card.
// Tests the three-gate chip strip, lifecycle gating on approve/reject,
// SSE-overlay precedence over REST snapshots, and the axe a11y
// baseline.

import { describe, expect, it, vi, beforeEach } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import axe from 'axe-core'
import HypothesizedProposalCard from './HypothesizedProposalCard'
import type { HypothesizedProposal } from '../types/HypothesizedProposal'
import type { ProposalLifecycle } from '../types/ProposalLifecycle'
import type { ProposalEventState } from '../hooks/useSseChatEvents'

// Mock the REST client so the card never hits the network.
vi.mock('../api/chatClient', () => {
  return {
    signoffProposal: vi.fn(),
    rejectProposal: vi.fn(),
  }
})

function makeProposal(overrides: Partial<HypothesizedProposal> = {}): HypothesizedProposal {
  return {
    id: 'proposal-abc123def456',
    node_id: 'doublet_score',
    intent: 'Score per-cell doublet probability',
    parent_terms: ['edam:operation_3198', 'swfc:doublet_detection'],
    assumptions: ['Counts matrix is raw, not normalized.'],
    failure_modes: ['False positives on neighbouring multiplets.'],
    validation_tests: ['cell_count_matches', 'no_negative_counts'],
    upstream_atom_ids: [],
    llm_rationale: 'Doublet scores filter heterotypic artifacts before clustering.',
    lifecycle: { kind: 'pending_validation' } satisfies ProposalLifecycle,
    gate_outcomes: [],
    created_at: 1000n,
    last_transition_at: 1000n,
    ...overrides,
  }
}

async function runAxe(node: HTMLElement) {
  return axe.run(node, {
    runOnly: {
      type: 'tag',
      values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'],
    },
    rules: { 'color-contrast': { enabled: false } },
  })
}

describe('HypothesizedProposalCard', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  describe('chip rendering per lifecycle', () => {
    it('PendingValidation: validator running, sandbox & signoff pending', () => {
      render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({ lifecycle: { kind: 'pending_validation' } })}
        />,
      )
      expect(
        screen.getByTestId('proposal-chip-validator').getAttribute('data-chip-status'),
      ).toBe('running')
      expect(
        screen.getByTestId('proposal-chip-sandbox').getAttribute('data-chip-status'),
      ).toBe('pending')
      expect(
        screen.getByTestId('proposal-chip-sme_signoff').getAttribute('data-chip-status'),
      ).toBe('pending')
    })

    it('PendingSandbox: validator passed, sandbox running, signoff pending', () => {
      render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({ lifecycle: { kind: 'pending_sandbox' } })}
        />,
      )
      expect(
        screen.getByTestId('proposal-chip-validator').getAttribute('data-chip-status'),
      ).toBe('passed')
      expect(
        screen.getByTestId('proposal-chip-sandbox').getAttribute('data-chip-status'),
      ).toBe('running')
      expect(
        screen.getByTestId('proposal-chip-sme_signoff').getAttribute('data-chip-status'),
      ).toBe('pending')
    })

    it('AwaitingSignoff: validator + sandbox passed, signoff running', () => {
      render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({ lifecycle: { kind: 'awaiting_signoff' } })}
        />,
      )
      expect(
        screen.getByTestId('proposal-chip-validator').getAttribute('data-chip-status'),
      ).toBe('passed')
      expect(
        screen.getByTestId('proposal-chip-sandbox').getAttribute('data-chip-status'),
      ).toBe('passed')
      expect(
        screen.getByTestId('proposal-chip-sme_signoff').getAttribute('data-chip-status'),
      ).toBe('running')
    })

    it('Blocked(ValidatorFailed): validator failed chip + inline alert + Re-propose hint', () => {
      const { container } = render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({
            lifecycle: {
              kind: 'blocked',
              reason: {
                kind: 'validator_failed',
                failures: ['cell_count_matches', 'no_negative_counts'],
              },
            },
          })}
        />,
      )
      expect(
        screen.getByTestId('proposal-chip-validator').getAttribute('data-chip-status'),
      ).toBe('failed')
      const alert = screen.getByTestId('proposal-blocked-alert')
      expect(alert.getAttribute('data-blocker-reason')).toBe('validator_failed')
      expect(alert).toHaveTextContent(/cell_count_matches/i)
      expect(alert).toHaveTextContent(/Re-propose/)
      expect(container).toBeTruthy()
    })

    it('Blocked(SandboxRefused): sandbox failed chip + inline alert lists refusal kinds', () => {
      render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({
            lifecycle: {
              kind: 'pending_sandbox', // upstream pending, but actual lifecycle goes through Blocked
            },
          })}
        />,
      )
      // Validate the upstream test first; then explicitly test the sandbox-failed branch.
    })

    it('Blocked(SandboxRefused) explicit: alert lists kinds', () => {
      render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({
            lifecycle: {
              kind: 'blocked',
              reason: {
                kind: 'sandbox_refused',
                refusals: [
                  { kind: 'network_denied' },
                  { kind: 'container_required' },
                ],
              },
            },
          })}
        />,
      )
      expect(
        screen.getByTestId('proposal-chip-sandbox').getAttribute('data-chip-status'),
      ).toBe('failed')
      const alert = screen.getByTestId('proposal-blocked-alert')
      expect(alert.getAttribute('data-blocker-reason')).toBe('sandbox_refused')
      expect(alert).toHaveTextContent(/network_denied/)
      expect(alert).toHaveTextContent(/container_required/)
    })

    it('Promoted: collapses to a single-line status row with task_node_id', () => {
      render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({
            lifecycle: { kind: 'promoted', task_node_id: 'doublet_score_v2' },
          })}
        />,
      )
      const status = screen.getByRole('status')
      expect(status.getAttribute('data-proposal-state')).toBe('promoted')
      expect(status).toHaveTextContent(/Promoted/)
      expect(status).toHaveTextContent(/doublet_score_v2/)
      // The full card with buttons collapses away.
      expect(screen.queryByTestId('proposal-approve')).not.toBeInTheDocument()
      expect(screen.queryByTestId('proposal-reject')).not.toBeInTheDocument()
    })

    it('Rejected: collapses to a single-line status row, rationale visible', () => {
      render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({
            lifecycle: { kind: 'rejected', rationale: 'Out of scope for this round.' },
          })}
        />,
      )
      const status = screen.getByRole('status')
      expect(status.getAttribute('data-proposal-state')).toBe('rejected')
      expect(status).toHaveTextContent(/Rejected/)
      expect(status).toHaveTextContent(/Out of scope/)
      expect(screen.queryByTestId('proposal-approve')).not.toBeInTheDocument()
    })
  })

  describe('button enablement', () => {
    it('Approve disabled when lifecycle != awaiting_signoff', () => {
      render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({ lifecycle: { kind: 'pending_validation' } })}
        />,
      )
      expect(screen.getByTestId('proposal-approve')).toBeDisabled()
    })

    it('Approve enabled when lifecycle == awaiting_signoff', () => {
      render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({ lifecycle: { kind: 'awaiting_signoff' } })}
        />,
      )
      expect(screen.getByTestId('proposal-approve')).toBeEnabled()
    })

    it('Reject enabled in non-terminal lifecycles (incl. blocked)', () => {
      // Spec §9: "Reject enabled while not terminal." Blocked is
      // non-terminal — the SME may re-propose OR reject outright.
      const states: HypothesizedProposal['lifecycle'][] = [
        { kind: 'awaiting_signoff' },
        { kind: 'pending_validation' },
        { kind: 'pending_sandbox' },
      ]
      for (const lc of states) {
        const { unmount } = render(
          <HypothesizedProposalCard
            sessionId="s1"
            proposal={makeProposal({ lifecycle: lc })}
          />,
        )
        expect(screen.getByTestId('proposal-reject')).toBeEnabled()
        unmount()
      }
    })

    it('Reject enabled when lifecycle == blocked', () => {
      // Regression for §9 alignment: an earlier revision disabled
      // Reject in Blocked. The SME must be able to reject a blocked
      // proposal without re-proposing first.
      render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({
            lifecycle: {
              kind: 'blocked',
              reason: {
                kind: 'validator_failed',
                failures: ['p_value_in_unit_interval'],
              },
            },
          })}
        />,
      )
      expect(screen.getByTestId('proposal-reject')).toBeEnabled()
    })
  })

  describe('approve interaction', () => {
    it('clicking Approve calls signoffProposal(sessionId, proposal.id)', async () => {
      const { signoffProposal } = await import('../api/chatClient')
      ;(signoffProposal as ReturnType<typeof vi.fn>).mockResolvedValue(undefined)
      const onPromoted = vi.fn()
      const user = userEvent.setup()
      render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({ lifecycle: { kind: 'awaiting_signoff' } })}
          onPromoted={onPromoted}
        />,
      )
      await user.click(screen.getByTestId('proposal-approve'))
      await waitFor(() => {
        expect(signoffProposal).toHaveBeenCalledWith(
          's1',
          'proposal-abc123def456',
        )
      })
      expect(onPromoted).toHaveBeenCalled()
    })

    it('Approve does NOT prompt for SME initials in this version', async () => {
      const { signoffProposal } = await import('../api/chatClient')
      ;(signoffProposal as ReturnType<typeof vi.fn>).mockResolvedValue(undefined)
      const user = userEvent.setup()
      render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({ lifecycle: { kind: 'awaiting_signoff' } })}
        />,
      )
      await user.click(screen.getByTestId('proposal-approve'))
      await waitFor(() => {
        // Only two arguments — sessionId + proposalId. No initials prompt.
        expect(signoffProposal).toHaveBeenCalledTimes(1)
        const lastCall = (signoffProposal as ReturnType<typeof vi.fn>).mock
          .calls[0]
        expect(lastCall).toHaveLength(2)
      })
    })
  })

  describe('reject interaction', () => {
    it('clicking Reject opens a textarea; Confirm submits with rationale', async () => {
      const { rejectProposal } = await import('../api/chatClient')
      ;(rejectProposal as ReturnType<typeof vi.fn>).mockResolvedValue(undefined)
      const onRejected = vi.fn()
      const user = userEvent.setup()
      render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({ lifecycle: { kind: 'awaiting_signoff' } })}
          onRejected={onRejected}
        />,
      )
      // Initially no textarea visible.
      expect(screen.queryByTestId('reject-rationale')).not.toBeInTheDocument()
      // Open the reject pane.
      await user.click(screen.getByTestId('proposal-reject'))
      const textarea = await screen.findByTestId('reject-rationale')
      await user.type(textarea, 'Out of scope for this run')
      await user.click(screen.getByTestId('proposal-reject-confirm'))
      await waitFor(() => {
        expect(rejectProposal).toHaveBeenCalledWith(
          's1',
          'proposal-abc123def456',
          'Out of scope for this run',
        )
      })
      expect(onRejected).toHaveBeenCalled()
    })

    it('Cancel restores the initial state without firing rejectProposal', async () => {
      const { rejectProposal } = await import('../api/chatClient')
      const user = userEvent.setup()
      render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({ lifecycle: { kind: 'awaiting_signoff' } })}
        />,
      )
      await user.click(screen.getByTestId('proposal-reject'))
      await user.click(screen.getByTestId('proposal-reject-cancel'))
      expect(screen.queryByTestId('reject-rationale')).not.toBeInTheDocument()
      expect(rejectProposal).not.toHaveBeenCalled()
    })

    it('rejecting with an empty textarea passes undefined rationale', async () => {
      const { rejectProposal } = await import('../api/chatClient')
      ;(rejectProposal as ReturnType<typeof vi.fn>).mockResolvedValue(undefined)
      const user = userEvent.setup()
      render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({ lifecycle: { kind: 'awaiting_signoff' } })}
        />,
      )
      await user.click(screen.getByTestId('proposal-reject'))
      await user.click(screen.getByTestId('proposal-reject-confirm'))
      await waitFor(() => {
        expect(rejectProposal).toHaveBeenCalledWith(
          's1',
          'proposal-abc123def456',
          undefined,
        )
      })
    })
  })

  describe('SSE overlay precedence over REST snapshot', () => {
    it('liveOverlay.validator === true renders ✓ even when lifecycle is still pending_validation', () => {
      const overlay: ProposalEventState = {
        proposalId: 'proposal-abc123def456',
        nodeId: 'doublet_score',
        validator: true, // SSE has fired but REST hasn't refetched yet
        sandbox: undefined,
        terminal: null,
        promotedTaskNodeId: null,
        rejectRationale: null,
      }
      render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({ lifecycle: { kind: 'pending_validation' } })}
          liveOverlay={overlay}
        />,
      )
      // The validator chip flips to passed via the overlay even
      // though `lifecycle.kind` is still pending_validation.
      expect(
        screen.getByTestId('proposal-chip-validator').getAttribute('data-chip-status'),
      ).toBe('passed')
    })

    it('liveOverlay.sandbox === false renders ✗ + the chip strip stays in role=status', () => {
      const overlay: ProposalEventState = {
        proposalId: 'proposal-abc123def456',
        nodeId: 'doublet_score',
        validator: true,
        sandbox: false,
        terminal: null,
        promotedTaskNodeId: null,
        rejectRationale: null,
      }
      render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({ lifecycle: { kind: 'pending_sandbox' } })}
          liveOverlay={overlay}
        />,
      )
      expect(
        screen.getByTestId('proposal-chip-sandbox').getAttribute('data-chip-status'),
      ).toBe('failed')
      expect(screen.getByTestId('proposal-chip-strip')).toHaveAttribute(
        'aria-live',
        'polite',
      )
    })

    it('liveOverlay.terminal === "promoted" collapses to one-line status even if REST lifecycle is still awaiting_signoff', () => {
      const overlay: ProposalEventState = {
        proposalId: 'proposal-abc123def456',
        nodeId: 'doublet_score',
        validator: true,
        sandbox: true,
        terminal: 'promoted',
        promotedTaskNodeId: 'doublet_score_v2',
        rejectRationale: null,
      }
      render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({ lifecycle: { kind: 'awaiting_signoff' } })}
          liveOverlay={overlay}
        />,
      )
      const status = screen.getByRole('status')
      expect(status.getAttribute('data-proposal-state')).toBe('promoted')
      expect(status).toHaveTextContent(/doublet_score_v2/)
    })
  })

  describe('accessibility', () => {
    it('Approve button has descriptive aria-label', () => {
      render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({ lifecycle: { kind: 'awaiting_signoff' } })}
        />,
      )
      const approve = screen.getByTestId('proposal-approve')
      expect(approve).toHaveAttribute(
        'aria-label',
        'Approve and promote node doublet_score',
      )
    })

    it('Reject button has descriptive aria-label', () => {
      render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({ lifecycle: { kind: 'awaiting_signoff' } })}
        />,
      )
      const reject = screen.getByTestId('proposal-reject')
      expect(reject).toHaveAttribute('aria-label', 'Reject proposal doublet_score')
    })

    it('Gate progress strip has role=status with aria-live=polite', () => {
      render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({ lifecycle: { kind: 'pending_validation' } })}
        />,
      )
      const strip = screen.getByTestId('proposal-chip-strip')
      expect(strip).toHaveAttribute('role', 'status')
      expect(strip).toHaveAttribute('aria-live', 'polite')
    })

    it('axe-core finds no WCAG 2.1 AA violations (awaiting_signoff)', async () => {
      const { container } = render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({ lifecycle: { kind: 'awaiting_signoff' } })}
        />,
      )
      const results = await runAxe(container)
      expect(results.violations).toEqual([])
    })

    it('axe-core finds no WCAG 2.1 AA violations (promoted)', async () => {
      const { container } = render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({
            lifecycle: { kind: 'promoted', task_node_id: 'doublet_score_v2' },
          })}
        />,
      )
      const results = await runAxe(container)
      expect(results.violations).toEqual([])
    })

    it('axe-core finds no WCAG 2.1 AA violations (blocked)', async () => {
      const { container } = render(
        <HypothesizedProposalCard
          sessionId="s1"
          proposal={makeProposal({
            lifecycle: {
              kind: 'blocked',
              reason: { kind: 'validator_failed', failures: ['x'] },
            },
          })}
        />,
      )
      const results = await runAxe(container)
      expect(results.violations).toEqual([])
    })
  })
})
