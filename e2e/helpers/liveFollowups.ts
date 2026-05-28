import type { LiveFollowup } from './types'

export interface LiveFollowupPick {
  kind: 'scenario' | 'fallback'
  key: string
  reply: string
}

export function chooseLiveFollowup(
  followups: readonly LiveFollowup[] | undefined,
  assistantText: string,
  usedTriggers: ReadonlySet<string>,
  fallbackReply: string,
): LiveFollowupPick {
  for (const followup of followups ?? []) {
    if (usedTriggers.has(followup.trigger)) continue

    const trigger = compileScenarioTrigger(followup.trigger)
    if (trigger.test(assistantText)) {
      return {
        kind: 'scenario',
        key: followup.trigger,
        reply: followup.reply,
      }
    }
  }

  return {
    kind: 'fallback',
    key: 'fallback',
    reply: fallbackReply,
  }
}

function compileScenarioTrigger(pattern: string): RegExp {
  if (pattern.startsWith('(?i)')) {
    return new RegExp(pattern.slice(4), 'i')
  }
  return new RegExp(pattern)
}
