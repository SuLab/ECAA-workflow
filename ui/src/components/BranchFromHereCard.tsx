// Inline turn-card the user can dispatch to fork the current session
// into a branched alternative without losing the existing one. Wired
// to the branch_session tool.
//
// Rendered by AssistantTurnCard when the session has emitted at least
// one package and the state admits branching (emitted or amending —
// not blocked, greeting, or intake). The non-task-scoped entry point
// complements the existing TaskDetailDrawer "Explore in a branch" path.

import { useState } from 'react'
import { useAsync } from '../hooks/useAsync'
import { CardContainer } from './primitives/CardContainer'
import { OptionalTextarea } from './primitives/OptionalTextarea'
import { SubmitCancelRow } from './primitives/SubmitCancelRow'
import { CARD_PALETTES } from '../styles/palettes'

interface Props {
  /**
   * Click handler — receives the optional rationale prose. Returns a
   * promise so the card can show a submitting state. The conversation
   * pane is responsible for routing the SME to the new branched
   * session id once the server returns it.
   */
  onBranch: (rationale?: string) => void | Promise<void>
  disabled?: boolean
}

export default function BranchFromHereCard({ onBranch, disabled }: Props) {
  const [expanded, setExpanded] = useState(false)
  const [rationale, setRationale] = useState('')
  const { busy, run } = useAsync()
  const branchPalette = CARD_PALETTES.branch

  const handleClick = () => {
    setExpanded(true)
  }
  const handleCancel = () => {
    setExpanded(false)
    setRationale('')
  }
  const handleConfirm = async () => {
    await run(async () => {
      const trimmed = rationale.trim()
      await onBranch(trimmed ? trimmed : undefined)
    })
    setExpanded(false)
    setRationale('')
  }

  return (
    <CardContainer
      palette="branch"
      role="region"
      ariaLabel="Branch from this session"
      title="Want to try an alternative without losing this analysis?"
      dataAttrs={{ 'data-branch-card-state': expanded ? 'open' : 'closed' }}
    >
      <p
        style={{
          margin: 0,
          fontSize: '0.81rem',
          color: 'var(--color-branch-fg)',
          lineHeight: 1.4,
          marginBottom: '0.6rem',
        }}
      >
        Branching creates a parallel copy of this analysis from this
        point forward. You can try a different approach in the new branch
        without affecting this one — switch between them anytime.
      </p>

      {!expanded && (
        <button
          type="button"
          onClick={handleClick}
          disabled={disabled}
          style={{
            padding: '0.45rem 0.9rem',
            background: branchPalette.accent,
            color: 'var(--color-text-on-accent)',
            border: 'none',
            borderRadius: 6,
            cursor: disabled ? 'not-allowed' : 'pointer',
            fontSize: '0.8rem',
            fontWeight: 600,
            opacity: disabled ? 0.6 : 1,
          }}
        >
          Branch from here
        </button>
      )}

      {expanded && (
        <div data-branch-panel="open">
          <OptionalTextarea
            label="Optional rationale"
            ariaLabel="Optional branching rationale"
            value={rationale}
            onChange={setRationale}
            disabled={busy}
            rows={2}
            placeholder="e.g. trying a different normalization without losing the current run"
          />
          <SubmitCancelRow
            palette="branch"
            submitLabel="Create branch"
            cancelLabel="Cancel"
            onSubmit={handleConfirm}
            onCancel={handleCancel}
            busy={busy}
          />
        </div>
      )}
    </CardContainer>
  )
}
