/**
 * Integration test: BranchFromHereCard wiring into AssistantTurnCard.
 *
 * Verifies that the card:
 *  1. Renders inside AssistantTurnCard when `isLatest=true` and `onBranch`
 *     is supplied (i.e., session state admits branching).
 *  2. Does NOT render when `onBranch` is absent (parent withholds it for
 *     states that don't admit branching — Blocked, Greeting, Intake).
 *  3. Does NOT render on non-latest turns even when `onBranch` is supplied.
 *  4. Expands to reveal the rationale textarea + Create branch button on click.
 *  5. Calls `onBranch` with the trimmed rationale on confirm.
 *
 * The session-state guard lives in ConversationPane (it decides whether to
 * pass `onBranch` to ChatTimeline/AssistantTurnCard). AssistantTurnCard
 * renders the card purely based on prop presence + `isLatest`, so these
 * tests cover the full wiring contract without needing to mount ConversationPane.
 */
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import AssistantTurnCard from '../AssistantTurnCard'
import type { Turn } from '../../types'

function makeAssistantTurn(content: string): Turn {
  return {
    turn_id: 'a-1',
    role: 'assistant',
    content,
    intent: null,
    tool_calls: [],
    quick_replies: [],
    confirmation_card: null,
    timestamp: '2026-05-22T00:00:00Z',
  }
}

describe('BranchFromHereCard integration with AssistantTurnCard', () => {
  it('renders BranchFromHereCard when isLatest=true and onBranch is supplied (session admits branching)', () => {
    render(
      <AssistantTurnCard
        turn={makeAssistantTurn('Package emitted successfully.')}
        isLatest
        pillStatusLine={null}
        onConfirm={vi.fn()}
        onReject={vi.fn()}
        onQuickReply={vi.fn()}
        onBranch={vi.fn()}
      />,
    )
    expect(screen.getByRole('button', { name: /branch from here/i })).toBeInTheDocument()
  })

  it('does not render BranchFromHereCard when onBranch is absent (state does not admit branching — Blocked, Greeting, Intake)', () => {
    render(
      <AssistantTurnCard
        turn={makeAssistantTurn('Session is blocked.')}
        isLatest
        pillStatusLine={null}
        onConfirm={vi.fn()}
        onReject={vi.fn()}
        onQuickReply={vi.fn()}
        // onBranch intentionally omitted — ConversationPane withholds it
        // when stateKind is 'blocked', 'greeting', or pre-emission.
      />,
    )
    expect(screen.queryByRole('button', { name: /branch from here/i })).not.toBeInTheDocument()
  })

  it('does not render BranchFromHereCard on non-latest turns even when onBranch is supplied', () => {
    render(
      <AssistantTurnCard
        turn={makeAssistantTurn('Earlier turn in the transcript.')}
        isLatest={false}
        pillStatusLine={null}
        onConfirm={vi.fn()}
        onReject={vi.fn()}
        onQuickReply={vi.fn()}
        onBranch={vi.fn()}
      />,
    )
    expect(screen.queryByRole('button', { name: /branch from here/i })).not.toBeInTheDocument()
  })

  it('clicking "Branch from here" expands the rationale panel', () => {
    render(
      <AssistantTurnCard
        turn={makeAssistantTurn('Package emitted.')}
        isLatest
        pillStatusLine={null}
        onConfirm={vi.fn()}
        onReject={vi.fn()}
        onQuickReply={vi.fn()}
        onBranch={vi.fn()}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: /branch from here/i }))
    expect(screen.getByLabelText(/optional branching rationale/i)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /create branch/i })).toBeInTheDocument()
  })

  it('calls onBranch with trimmed rationale on confirm', async () => {
    const onBranch = vi.fn().mockResolvedValue(undefined)
    render(
      <AssistantTurnCard
        turn={makeAssistantTurn('Package emitted.')}
        isLatest
        pillStatusLine={null}
        onConfirm={vi.fn()}
        onReject={vi.fn()}
        onQuickReply={vi.fn()}
        onBranch={onBranch}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: /branch from here/i }))
    const textarea = screen.getByLabelText(/optional branching rationale/i)
    fireEvent.change(textarea, { target: { value: '  try a different aligner  ' } })
    fireEvent.click(screen.getByRole('button', { name: /create branch/i }))
    await waitFor(() => expect(onBranch).toHaveBeenCalledWith('try a different aligner'))
  })

  it('calls onBranch with undefined when rationale is blank', async () => {
    const onBranch = vi.fn().mockResolvedValue(undefined)
    render(
      <AssistantTurnCard
        turn={makeAssistantTurn('Package emitted.')}
        isLatest
        pillStatusLine={null}
        onConfirm={vi.fn()}
        onReject={vi.fn()}
        onQuickReply={vi.fn()}
        onBranch={onBranch}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: /branch from here/i }))
    fireEvent.click(screen.getByRole('button', { name: /create branch/i }))
    await waitFor(() => expect(onBranch).toHaveBeenCalledWith(undefined))
  })
})
