// Generic merge-by-id helper used by the 60s reconciliation poll in
// `useConversation`. The poll fetches the server-side transcript and
// merges it into the locally-rendered turns array; without this,
// optimistic user-turn appends (`appendTurn`-style renders before the
// server has acked the new turn) would be wiped out on the next tick.
//
// This helper began as an append-only dedup, was upgraded to merge-by-id
// with field-level reconciliation, and now walks REMOTE order as the
// canonical chronology. The earlier "remote-only items appended at the
// end" rule misordered the transcript when the SSE `turn_appended`
// stream beat the persisted user turn into local state (e.g. when an
// out-of-band POST /turn arrives via a sibling tab or test harness):
// the assistant turn would land via SSE first, and then the 60s poll
// would shove the missing user turn after the assistant reply that
// answered it. Walking REMOTE in order keeps the persisted transcript
// authoritative on chronology.

/**
 * Merge `local` and `remote` lists keyed on `key`.
 *
 * REMOTE order is canonical. The result walks `remote` in its given
 * order and, for each item, either takes the remote value or merges
 * with the matching local item via `{...local, ...remote }` (server
 * fields win on overlap; local-only fields survive).
 *
 * Local-only items (optimistic appends not yet on server) are kept at
 * the end in their original local order. The shared-userTurnId hint
 * means an SME-typed turn round-trips through this branch only during
 * the milliseconds between the optimistic append and the POST /turn
 * response.
 *
 * Fast paths (return `local` reference unchanged):
 * - both empty / remote empty / local empty (legacy)
 * - same length AND every (key, fields) referentially equal (new):
 *   skips the per-element `{...local, ...remote}` spread so React's
 *   `===` short-circuits downstream don't get defeated by a fresh
 *   array + fresh object identities for a no-op poll tick.
 */
export function mergeBy<T>(local: T[], remote: T[], key: keyof T): T[] {
  if (local.length === 0 && remote.length === 0) return local
  if (remote.length === 0) return local
  if (local.length === 0) return [...remote]
  // Fast path: if remote walks the same keys in the same order AND every
  // matching field is referentially equal, return `local` so React's
  // referential-equality short-circuits downstream.
  if (remote.length === local.length) {
    let identical = true
    for (let i = 0; i < remote.length; i += 1) {
      const r = remote[i]
      const l = local[i]
      if (r == null || l == null || r[key] !== l[key]) {
        identical = false
        break
      }
      // Shallow field-equality check via Object.keys union — server fields
      // win on overlap, so if any remote field differs from local's value,
      // we'd produce a different merged element.
      const allKeys = new Set([...Object.keys(l as object), ...Object.keys(r as object)])
      for (const k of allKeys) {
        if ((l as Record<string, unknown>)[k] !== (r as Record<string, unknown>)[k]) {
          identical = false
          break
        }
      }
      if (!identical) break
    }
    if (identical) return local
  }
  const localByKey = new Map<unknown, T>()
  for (const l of local) {
    localByKey.set(l[key], l)
  }
  // Walk REMOTE in order so the persisted server chronology is the
  // canonical sequence. An append-end merge would misorder any user
  // turn that arrived locally AFTER its own assistant reply.
  const merged: T[] = remote.map((r) => {
    const l = localByKey.get(r[key])
    if (l === undefined) return r
    localByKey.delete(r[key])
    // Shallow spread: server is canonical on overlapping fields.
    // Local-only fields (e.g. an SME draft attached to a turn the
    // server hasn't seen yet) survive.
    return { ...l, ...r }
  })
  if (localByKey.size === 0) return merged
  // Local-only items (optimistic appends the server hasn't ack'd) keep
  // their relative order; appended after the canonical remote chronology.
  const localOnly: T[] = []
  for (const l of local) {
    if (localByKey.has(l[key])) localOnly.push(l)
  }
  return [...merged, ...localOnly]
}
