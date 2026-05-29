/**
 * `CompositionOutcomeBanner`.
 *
 * Surfaces non-Validated v4 outcomes (Refusal, NovelNodeSpec,
 * DraftDag, PartialDag) above the conversation pane so the SME
 * notices composition-blocking issues without having to open the
 * Composition tab. Renders nothing for v1/v2/v3 sessions and for
 * v4 sessions whose outcome is `validated_executable_dag` with no
 * unresolved assumptions.
 *
 * The banner is informational + a single action: click to open the
 * Composition tab. The actual resolution (resolve assumption /
 * confirm adapter / branch refusal) lives in the tab.
 */

import { useState } from 'react'
import { useCancelableEffect } from '../hooks/useCancelableFetch'
import {
  getComposeOutcome,
  type ComposeOutcomePayload,
} from '../api/chatClient'

interface Props {
  sessionId: string | null
  /** Bumps when last_activity changes so the banner re-fetches. */
  refreshKey?: string
}

export default function CompositionOutcomeBanner({
  sessionId,
  refreshKey,
}: Props): JSX.Element | null {
  const [outcome, setOutcome] = useState<ComposeOutcomePayload | null>(null)

  useCancelableEffect(async ({ cancelled }) => {
    if (!sessionId) {
      setOutcome(null)
      return
    }
    try {
      const result = await getComposeOutcome(sessionId)
      if (!cancelled()) setOutcome(result)
    } catch {
      if (!cancelled()) setOutcome(null)
    }
  }, [sessionId, refreshKey])

  if (!outcome) return null
  const banner = bannerForOutcome(outcome)
  if (!banner) return null

  return (
    <div
      role="status"
      aria-live="polite"
      style={{
        padding: '0.55rem 0.85rem',
        margin: '0.4rem 0.75rem',
        background: banner.bg,
        border: `1px solid ${banner.border}`,
        borderRadius: 6,
        color: banner.fg,
        fontSize: '0.78rem',
        display: 'flex',
        alignItems: 'center',
        gap: '0.5rem',
      }}
    >
      <span aria-hidden style={{ fontSize: '1rem' }}>
        {banner.icon}
      </span>
      <span style={{ flex: 1 }}>
        <strong>{banner.title}</strong>{' '}
        <span style={{ color: 'var(--color-text-secondary)' }}>
          {banner.detail}
        </span>
      </span>
      <button
        type="button"
        onClick={() =>
          window.dispatchEvent(
            new CustomEvent('ecaax:switch-tab', {
              detail: { tab: 'composition' },
            }),
          )
        }
        style={{
          padding: '0.25rem 0.7rem',
          fontSize: '0.74rem',
          background: 'var(--color-surface-1)',
          border: `1px solid ${banner.border}`,
          borderRadius: 4,
          color: banner.fg,
          cursor: 'pointer',
          fontWeight: 600,
        }}
      >
        Open Composition tab
      </button>
    </div>
  )
}

interface BannerSpec {
  title: string
  detail: string
  bg: string
  border: string
  fg: string
  icon: string
}

function bannerForOutcome(o: ComposeOutcomePayload): BannerSpec | null {
  switch (o.variant) {
    case 'refusal':
      return {
        title: 'Composition refused',
        detail:
          'The proof-carrying composer refused to produce a DAG. Open the Composition tab for the refusal report and recovery options (branch / amend policy).',
        bg: 'var(--color-danger-bg)',
        border: 'var(--color-danger-border)',
        fg: 'var(--color-danger-fg)',
        icon: '⛔',
      }
    case 'novel_node_spec':
      return {
        title: 'New task proposed',
        detail:
          'The planner proposed a hypothesized node that needs your review. Open the Composition tab to accept it as a draft or reject it.',
        bg: 'var(--color-info-bg)',
        border: 'var(--color-info-border)',
        fg: 'var(--color-info-fg)',
        icon: '🧪',
      }
    case 'draft_dag':
      return {
        title: 'Draft composition (not production-ready)',
        detail: `${o.assumption_count} unresolved assumption${o.assumption_count === 1 ? '' : 's'} block${o.assumption_count === 1 ? 's' : ''} promotion to ValidatedExecutableDag. Resolve them in the Composition tab.`,
        bg: 'var(--color-warning-bg)',
        border: 'var(--color-warning-border)',
        fg: 'var(--color-warning-fg)',
        icon: '⚠',
      }
    case 'partial_dag': {
      const gapCount = o.unresolved_gaps?.length ?? 0
      return {
        title: 'Composition incomplete',
        detail: `${gapCount} unresolved gap${gapCount === 1 ? '' : 's'} prevent${gapCount === 1 ? 's' : ''} the planner from producing an executable DAG. Open the Composition tab for the missing-port list and remediation hints.`,
        bg: 'var(--color-warning-bg)',
        border: 'var(--color-warning-border)',
        fg: 'var(--color-warning-fg)',
        icon: '⚠',
      }
    }
    case 'validated_executable_dag':
      // Quiet success — only surface a banner when there are
      // unresolved assumptions the SME should review.
      if (o.assumption_count > 0) {
        return {
          title: 'Composition validated, with assumptions',
          detail: `${o.assumption_count} unresolved assumption${o.assumption_count === 1 ? '' : 's'} accompany the executable DAG. Review them in the Composition tab before running.`,
          bg: 'var(--color-info-bg)',
          border: 'var(--color-info-border)',
          fg: 'var(--color-info-fg)',
          icon: 'ⓘ',
        }
      }
      return null
    default:
      return null
  }
}
