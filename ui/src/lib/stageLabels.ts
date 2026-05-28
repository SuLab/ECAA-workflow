// Stage-id → SME-readable label translation.
//
// Internal stage identifiers follow a `discover_*` / `validate_*` /
// `select_*` prefix convention useful for the builder and the agent, but
// exposing those raw strings to an SME violates the "no internal vocabulary
// in user-facing text" rule from `crates/conversation/src/prompt_role.txt`.
// This helper strips the prefix and humanizes the remainder.
//
// Behaviorally identical to `stage_id_to_human_label` in
// `crates/core/src/stage_labels.rs` — any change here requires the matching
// Rust update + shared fixture coverage in the sme-copy-linter suite.

const KNOWN_PREFIXES = ['discover_', 'validate_', 'select_'] as const

export function stageIdToLabel(stageId: string): string {
  const base = stripKnownPrefix(stageId)
  const spaced = base.replace(/_/g, ' ')
  return capitalizeFirst(spaced)
}

function stripKnownPrefix(stageId: string): string {
  for (const prefix of KNOWN_PREFIXES) {
    if (stageId.startsWith(prefix)) {
      return stageId.slice(prefix.length)
    }
  }
  return stageId
}

function capitalizeFirst(s: string): string {
  if (s.length === 0) return s
  return s.charAt(0).toUpperCase() + s.slice(1)
}
