import { memo } from 'react'
import type { Turn } from '../types'

interface Props {
  turn: Turn
}

// React.memo'd because ChatTimeline re-renders on every streaming
// token; without memoization every UserTurnCard re-renders even
// though its props (turn: Turn) are stable.
function UserTurnCardImpl({ turn }: Props) {
  return (
    <article
      aria-label="Your message"
      style={{
        alignSelf: 'flex-end',
        maxWidth: '92%',
        padding: '0.6rem 0.85rem',
        background: 'var(--color-user-bubble-bg)',
        color: 'var(--color-user-bubble-fg)',
        borderRadius: 12,
        borderTopRightRadius: 4,
        fontSize: '0.85rem',
        lineHeight: 1.5,
        whiteSpace: 'pre-wrap',
      }}
    >
      {turn.content}
    </article>
  )
}

export default memo(UserTurnCardImpl)
