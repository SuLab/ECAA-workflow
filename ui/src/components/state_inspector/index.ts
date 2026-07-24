// Barrel re-exports so callers + the parent StateInspectorPane can
// import every tab body from one path.

export { PlanTab } from './PlanTab'
export { StateTab } from './StateTab'
export { DocumentsPane } from './DocumentsTab'
export { JobsFeed } from './JobsTab'
export { MetricsTable, mergePilotStates } from './MetricsTab'
export { HistoryPane } from './HistoryTab'
export { FiguresPane } from './FiguresTab'
export { DashboardPane } from './DashboardTab'
export { DecisionsTab } from './DecisionsTab'
export { CompareTab } from './CompareTab'
export { InputsTab } from './InputsTab'
export { CompositionTab } from './CompositionTab'
// v4 P2 / F18 — typed verifier decision substrate (read-only).
export { VerifierDecisionsTab } from './VerifierDecisionsTab'
// V3+v4 residuals closure repair-strategy proposals tab.
export { RepairsTab } from './RepairsTab'
// Runtime claim-verification rollup (claim_extractor + claim_verifier
// per completed task). Distinct from `VerifierDecisionsTab`, which
// surfaces composer-time port-unification trace events.
export { ClaimsTab } from './ClaimsTab'
// Multi-analyst ensemble robustness rollup (opt-in ensemble mode):
// cross-method/cross-lens agreement, per-entity robustness class, and
// the deduplicated literature union. Distinct from Claims above — that
// verifies a single narrative against its own result table; this one
// compares across independently-run analyst variants.
export { EnsembleTab } from './EnsembleTab'
export {
  PlaceholderPane,
  backgroundForKind,
  borderForKind,
  labelForKind,
  textForKind,
  METRICS_POLL_MS,
} from './common'

export type Tab =
  | 'plan'
  | 'state'
  | 'documents'
  | 'jobs'
  | 'metrics'
  | 'figures'
  | 'dashboard'
  | 'decisions'
  | 'history'
  | 'compare'
  | 'inputs'
  | 'composition'
  | 'verifier_decisions'
  | 'repairs'
  | 'claims'
  | 'ensemble'
  | 'reproducibility'

export interface TabConfig {
  id: Tab
  label: string
}

export const TABS: readonly TabConfig[] = [
  { id: 'plan', label: 'Plan' },
  // Composition sits next to Plan because it's the
  // canonical "outcome" surface for the same DAG the Plan tab
  // visualizes. SMEs need it to be discoverable; burying it last
  // hides it.
  { id: 'composition', label: 'Composition' },
  { id: 'state', label: 'Status' },
  { id: 'documents', label: 'Documents' },
  { id: 'inputs', label: 'Inputs' },
  { id: 'jobs', label: 'Progress' },
  { id: 'metrics', label: 'Performance' },
  { id: 'figures', label: 'Figures' },
  { id: 'dashboard', label: 'Dashboard' },
  { id: 'decisions', label: 'Decisions' },
  // V3+v4 residuals closure repair-strategy proposals
  // produced by the planner's gap-closure pipeline. Sits next to
  // Decisions because both surfaces are SME-action queues — one for
  // structured DAG mutations, one for completed mutations.
  { id: 'repairs', label: 'Repairs' },
  // Per-task runtime claim verification: claim_extractor +
  // claim_verifier rollup across every completed task that has a
  // narrative artifact AND an interpretation policy declaring a
  // `verifiableEntities` block. This is the SME-facing "did the
  // narrative match the tables?" surface — distinct from the
  // composer-time port-unification trace below.
  { id: 'claims', label: 'Claims' },
  // Multi-analyst ensemble robustness rollup: cross-method/cross-lens
  // agreement + per-entity robustness class. Opt-in (ensemble mode) —
  // renders an empty state when no ensemble ran for this session.
  { id: 'ensemble', label: 'Robustness' },
  // v4 P2 / F18 — typed port-unification trace from the v4 proof-
  // carrying composer. Successful unifications form the edges of the
  // emitted DAG; failed ones are dead-end search branches the planner
  // correctly rejected. Distinct from the Claims tab above — that one
  // verifies agent narrative against result tables at runtime; this
  // one inspects the planner's compile-time decisions.
  { id: 'verifier_decisions', label: 'Composer trace' },
  { id: 'history', label: 'History' },
  { id: 'compare', label: 'Compare' },
  // Reproducibility surface: re-run the 6 audit-proof invariants with
  // the session HMAC secret (de-vacuifies claim-completeness) and drive
  // the deterministic `replay` re-verifier (Tier-1 integrity / Tier-2
  // full re-execute). Distinct from Claims (runtime narrative check) and
  // Composer trace (compile-time port unification) — this one attests
  // the emitted package against its own recorded provenance.
  { id: 'reproducibility', label: 'Reproducibility' },
]
