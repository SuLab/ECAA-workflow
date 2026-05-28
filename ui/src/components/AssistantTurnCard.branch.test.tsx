// R1.8: branch-affordance wiring on AssistantTurnCard. The card hosts
// a `BranchFromHereCard` footer when isLatest=true and `onBranch` is
// supplied; the footer is suppressed otherwise (older turns or
// pre-emission sessions).
import { describe, expect, test, vi } from 'vitest'
import { render, screen } from '@testing-library/react'
import AssistantTurnCard from './AssistantTurnCard'
import type { Turn } from '../types'

function makeAssistantTurn(content: string): Turn {
  return {
    turn_id: 'a-1',
    role: 'assistant',
    content,
    intent: null,
    tool_calls: [],
    quick_replies: [],
    confirmation_card: null,
    timestamp: '2026-05-18T00:00:00Z',
  }
}

describe('AssistantTurnCard branch-from-here wiring', () => {
  test('renders the BranchFromHereCard footer when isLatest and onBranch are set', () => {
    render(
      <AssistantTurnCard
        turn={makeAssistantTurn('emit complete')}
        isLatest
        pillStatusLine={null}
        onConfirm={vi.fn()}
        onReject={vi.fn()}
        onQuickReply={vi.fn()}
        onBranch={vi.fn()}
      />,
    )
    expect(
      screen.getByRole('button', { name: /Branch from here/i }),
    ).toBeInTheDocument()
  })

  test('omits the BranchFromHereCard footer when onBranch is undefined', () => {
    render(
      <AssistantTurnCard
        turn={makeAssistantTurn('emit complete')}
        isLatest
        pillStatusLine={null}
        onConfirm={vi.fn()}
        onReject={vi.fn()}
        onQuickReply={vi.fn()}
      />,
    )
    expect(
      screen.queryByRole('button', { name: /Branch from here/i }),
    ).not.toBeInTheDocument()
  })

  test('omits the BranchFromHereCard footer on non-latest turns', () => {
    render(
      <AssistantTurnCard
        turn={makeAssistantTurn('older turn')}
        isLatest={false}
        pillStatusLine={null}
        onConfirm={vi.fn()}
        onReject={vi.fn()}
        onQuickReply={vi.fn()}
        onBranch={vi.fn()}
      />,
    )
    expect(
      screen.queryByRole('button', { name: /Branch from here/i }),
    ).not.toBeInTheDocument()
  })
})
