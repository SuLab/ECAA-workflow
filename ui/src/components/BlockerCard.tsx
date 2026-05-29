import { useMemo, useState } from 'react'
import type { BlockerKind } from '../types'
import type { SandboxRefusalRecord } from '../types/SandboxRefusalRecord'
import type { BlockerAttempt, DiscoveryDecision } from '../api/chatClient'
import {
  getTaskBlocker,
  getTaskBlockerPayload,
  getTaskDecision,
  postAddRuntimePackage,
  postRerunScript,
  postSmeDecisions,
  postSmeSelection,
  setAutoApproveDiscoveries,
} from '../api/chatClient'
import { useAsync } from '../hooks/useAsync'
import { useCancelableEffect } from '../hooks/useCancelableFetch'
import { sanitizeForSme } from '../lib/smeText'
import { formatUSD } from '../lib/format'
import ExplainButton from './ExplainButton'
import { RadioRow, type RadioOption } from './primitives/RadioRow'
import RemediationSuggestionList from './RemediationSuggestionList'
import CompositionInfeasibleCard from './CompositionInfeasibleCard'

/**
 * Shape of the agent-written `runtime/outputs/<task>/blocker.json`
 * when it carries a structured-decision picker. Mirrors the shape the
 * agent's prompt contract documents + the server serves via
 * `GET /task/:id/blocker`.
 */
interface AgentBlockerJson {
  blocker_kind?: string
  decision_points_for_sme?: Array<{
    id: string
    question: string
    options?: Array<{
      id: string
      label?: string
      risk?: string
      consequence?: string
    }>
    default_if_unanswered?: string | null
  }>
  sme_pinned_method?: string
  missing_capability?: string
  recommended_substitute?: string
  summary?: string
  /// When the discovery scoring picked a single method as
  /// `top_candidate` but that method only covers a fraction of the
  /// cohort accessions, the agent also surfaces the cohort-spanning
  /// hybrid here. Each component contributes coverage; the union
  /// covers the full accession list. The BlockerCard prefers this
  /// over decision.json.top_candidate when present.
  top_candidate?: string
  top_candidate_components?: Array<{
    method_id: string
    score?: number
    covers_accessions?: string[]
  }>
  /// When the agent declares a one-click recovery action (e.g. rerun
  /// the wrapper script that crashed), the BlockerCard renders a
  /// secondary button that POSTs to /task/:task_id/rerun-script.
  recoverable_action?: {
    kind: 'rerun_script'
    rel_path: string
    label?: string
    description?: string
  }
}

export type StallResolution = 'resize' | 'retry' | 'abort'

interface Props {
  reason: string
  recoveryHint: string
  onUnblock: (resolution?: StallResolution) => void | Promise<void>
  disabled?: boolean
  /**
   * Typed blocker taxonomy. When present the card renders a
   * variant-specific title and tailors the button copy so the SME sees
   * whether the blocker is a validation miss, a metric gate, a missing
   * input, an agent or host error, or an SME selection gate.
   */
  blockerKind?: BlockerKind | null
  /**
   * Session id needed for the discovery-approval fetch path. When
   * present AND the blocker reason references a decision.json artifact,
   * the card fetches the decision, renders a radio-button list of
   * candidates with scores, and writes the SME's selection back to the
   * package via POST /task/:task_id/sme-selection before firing
   * onUnblock. When absent or when the blocker isn't a
   * discovery-approval blocker, the card degrades to the plain
   * reason + recovery + Unblock flow.
   */
  sessionId?: string | null
  /**
   * When the session has a pending disposition upstream of this
   * blocker (typically `results_review`), surface a one-line hint
   * pointing the SME at the disposition card. `null` suppresses the
   * hint.
   */
  relatedDispositionPath?: string | null
  relatedDispositionTaskId?: string | null
  /**
   * Explicit task-id override. Set when rendering the card from a
   * surface that already knows which task it's about (e.g. the
   * TaskDetailDrawer in the Plan tab). Overrides the regex-based
   * inference from `reason` and the `awaiting_structured_decision`
   * blockerKind.task_id path. Forces structured-decision rendering
   * so the picker fetches blocker.json for this task and surfaces
   * its decision_points_for_sme; an empty/missing blocker.json
   * gracefully renders the "no decision points yet" placeholder.
   */
  taskId?: string | null
}

function titleFor(kind: BlockerKind | null | undefined, isDiscovery: boolean): string {
  if (isDiscovery) return 'Method approval — please review candidates'
  if (!kind) return 'Something needs your attention before continuing'
  switch (kind.kind) {
    case 'data_shape_mismatch':
      return 'The data doesn\u2019t match the expected shape'
    case 'validation_failed':
      return 'A validation check failed'
    case 'metric_below_threshold':
      return 'A metric landed below the acceptable threshold'
    case 'missing_input':
      return 'An upstream task hasn\u2019t produced its output yet'
    case 'agent_error':
      return 'The agent hit an error it couldn\u2019t recover from'
    case 'host_error':
      return 'The host system reported an error'
    case 'awaiting_sme_selection':
      return 'Your input is needed to pick between alternatives'
    case 'pilot_oversize':
      return 'The pilot projection exceeds your cost ceiling'
    case 'stalled':
      return 'A running task looks stuck'
    case 'contract_violation':
      return 'A validation contract assertion failed'
    case 'runtime_capability_missing':
      return 'A required tool isn’t available in this environment'
    case 'awaiting_structured_decision':
      return 'Your input is needed on one or more decisions'
    case 'awaiting_sme_approval':
      return 'Your approval is needed before we commit to this method'
    case 'missing_artifact':
      return 'A required output file is missing or empty'
    case 'heartbeat_stalled':
      return 'A running task hasn’t reported progress in a while'
    case 'orphaned_by_crash':
      return 'This task was interrupted by a prior harness crash'
    case 'tool_error':
      return `${kind.envelope.library ?? 'A processing tool'} hit an error — pick a fix to retry`
    case 'image_digest_mismatch':
      return 'The container image digest doesn’t match what was pinned'
    case 'container_pull_failed':
      return 'Couldn’t pull the container image from the registry'
    case 'container_start_failed':
      return 'The container image pulled, but the runtime couldn’t start it'
    case 'runtime_missing':
      return 'No container runtime is available on this host'
    case 'sbom_emission_failed':
      return 'The supply-chain SBOM couldn’t be written'
    case 'network_policy_violation':
      return 'A task tried to use the network while the policy forbids it'
    case 'container_cache_corrupted':
      return 'The per-session container cache appears corrupted'
    case 'memory_exhausted':
      return 'The scheduler killed this task for exceeding its memory cap'
    case 'time_exceeded':
      return 'The scheduler killed this task for exceeding its wallclock cap'
    case 'replay_corruption':
      return 'A persisted record couldn’t be loaded — session history is incomplete'
    case 'image_digest_unresolved':
      return `Couldn't pin a registry digest for ${kind.image}:${kind.tag}`
    case 'composition_infeasible':
      return kind.unreachable_goal
        ? `The composer can’t reach ${kind.unreachable_goal} from your registry`
        : 'The composer can’t produce a valid plan from the current atom registry'
    case 'container_exited_abnormally':
      return kind.oom_killed
        ? `The container was OOM-killed (exit ${kind.exit_code})`
        : `The container exited with status ${kind.exit_code}`
    case 'slurm_runtime_unavailable':
      return `Partition ${kind.partition} doesn’t support ${kind.required}`
    case 'container_hung':
      // Distinct from heartbeat_stalled: the host is
      // healthy, only the in-container agent is wedged. Recovery is
      // "reap container only and rerun on the same host."
      return `${kind.runtime} container for ${kind.task_id} is alive but hung — host is fine`
    case 'iteration_did_not_converge':
      // Iterate-until atom hit max_iterations without
      // crossing the convergence threshold for the configured number
      // of consecutive passes. Recovery: raise threshold / accept
      // best so far / abort.
      return `${kind.task_id} ran ${kind.iterations_run} iterations — metric ${kind.last_metric.toFixed(3)} hasn't reached ${kind.threshold.toFixed(3)}`
    case 'sandbox_refused':
      // The harness `pre_dispatch_check` rejected a
      // generated-code task because its `TaskNode.implementation`
      // didn't satisfy the active `SandboxPolicy`. Recovery: amend
      // the policy bundle, request human review, or pivot to a
      // non-generated method.
      return 'Sandbox refused this task before dispatch'
    case 'adjudication_required':
      // The adjudication queue surfaced a transition
      // (`${kind.transition_kind}`) that needs a typed reviewer
      // decision. This title is interim text until the dedicated
      // card lands.
      return 'A reviewer decision is needed before this transition can proceed'
    case 'sandbox_required':
      // Atom-safety-policy the harness `enforce_safety_policy`
      // pre-dispatch gate refused this task because the atom requires
      // a sandbox level the executor doesn't offer. Recovery: switch
      // to an executor with the requested sandbox, or amend the atom
      // to relax its requirement.
      return `Atom ${kind.atom_id} needs ${kind.requested} sandbox — executor offers ${kind.available}`
    case 'network_policy_mismatch':
      // Atom-safety-policy the atom's declared NetworkPolicy
      // and the executor's policy disagree. Recovery: switch executor
      // or amend the atom's safety block.
      return `Atom ${kind.atom_id} network policy mismatch with executor`
    case 'provisioning_denied':
      // Atom-safety-policy the atom requested a runtime
      // package the policy hasn't yet allowed. Recovery: add the
      // package to atom.runtime_packages via the dedicated endpoint.
      return `Atom ${kind.atom_id} requested ${kind.package} from ${kind.registry} — denied`
    case 'schema_version_mismatch':
      // Config-manifest layout drift. The loader was registered for
      // `expected` but found `found` on disk; the MigrationRegistry
      // has no migrator covering the gap. Recovery: rebuild against
      // an upgraded loader, or migrate the manifest with the
      // `ecaa-workflow migrate-sessions`-style helper.
      return `${kind.config_kind} schema mismatch — expected ${kind.expected}, found ${kind.found}`
    case 'controlled_access_violation':
      return `Task ${kind.task_id} attempted ${kind.attempted_call} on controlled-access port ${kind.port_name}`
    case 'output_size_exceeded':
      return `Task ${kind.task_id} produced ${Number(kind.observed_bytes)} bytes (limit ${Number(kind.threshold_bytes)})`
    case 'patch_unparseable':
      return `Task ${kind.task_id} produced an unparseable DAG patch at ${kind.rejected_path}: ${kind.parse_error}`
    case 'clock_skew':
      return `Harness vs server clock skew is ${Number(kind.observed_secs)}s (threshold ${Number(kind.threshold_secs)}s)`
    case 'wall_clock_exceeded':
      return `Task ${kind.task_id} ran for ${Number(kind.observed_secs)}s (limit ${Number(kind.threshold_secs)}s)`
    case 'cancelled_by_amendment':
      return `Task ${kind.task_id} was cancelled by an amendment of ${kind.target_stage}`
    case 'provenance_commit_dropped':
      return `Provenance commit hook (${kind.trigger}) was skipped: ${kind.reason}`
    case 'turn_budget_exceeded':
      return 'Executor agent hit turn budget'
  }
  // Exhaustiveness: a new BlockerKind variant in core must extend this
  // switch; `tsc --noEmit` fails if `kind` is not narrowed to `never`.
  const _exhaustive: never = kind
  void _exhaustive
  return 'Something needs your attention before continuing'
}

function stallSignalSummary(kind: BlockerKind): string {
  if (kind.kind !== 'stalled') return ''
  const s = kind.signal
  switch (s.kind) {
    case 'cpu_starvation':
      return `Task ${kind.task_id} averaged ${s.avg_cpu_pct.toFixed(
        1,
      )}% CPU for ${Number(s.window_mins)} minutes.`
    case 'memory_pressure':
      return `Task ${kind.task_id} has been above ${s.pct.toFixed(
        1,
      )}% memory for ${Number(s.window_mins)} minutes.`
    case 'gpu_idle_during_training':
      return `Task ${kind.task_id} had an idle GPU during training for ${Number(s.window_mins)} minutes.`
    case 'runtime_over_expected': {
      const actual = Number(s.actual_secs)
      const expected = Number(s.expected_secs)
      const ratio = expected > 0 ? actual / expected : 0
      return `Task ${kind.task_id} has run ${actual}s, ${ratio.toFixed(1)}× the ${expected}s budget.`
    }
  }
  // Exhaustiveness: a new StallSignalWire variant must extend this
  // switch; `tsc --noEmit` fails if `s` is not narrowed to `never`.
  const _exhaustive: never = s
  void _exhaustive
  return `Task ${kind.task_id} stalled.`
}

function pilotOversizeSummary(kind: BlockerKind): string {
  if (kind.kind !== 'pilot_oversize') return ''
  return `Projected full-run cost is ${formatUSD(kind.projected_usd)}; your ceiling is ${formatUSD(kind.ceiling_usd)}.`
}

function missingArtifactSummary(kind: BlockerKind): string {
  if (kind.kind !== 'missing_artifact') return ''
  if (kind.missing_paths.length === 0) {
    return `Task ${kind.task_id} reported completion but no declared artifacts were found.`
  }
  if (kind.missing_paths.length === 1) {
    return `Task ${kind.task_id} marked complete, but ${kind.missing_paths[0]} is missing or empty.`
  }
  return `Task ${kind.task_id} marked complete, but ${kind.missing_paths.length} required artifacts are missing: ${kind.missing_paths.join(', ')}.`
}

function heartbeatStalledSummary(kind: BlockerKind): string {
  if (kind.kind !== 'heartbeat_stalled') return ''
  const mins = Math.round(Number(kind.last_heartbeat_secs_ago) / 60)
  return `Task ${kind.task_id} hasn't updated its heartbeat in ${mins} minute${mins === 1 ? '' : 's'}.`
}

function orphanedByCrashSummary(kind: BlockerKind): string {
  if (kind.kind !== 'orphaned_by_crash') return ''
  return `Task ${kind.task_id} was running when the harness exited (run ${kind.prior_harness_run_id.slice(0, 8)}, ${kind.last_dispatch_at}). Rerun to pick up where it left off.`
}

function memoryExhaustedSummary(kind: BlockerKind): string {
  if (kind.kind !== 'memory_exhausted') return ''
  const peak = kind.peak_memory_mb !== undefined ? `${Math.round(Number(kind.peak_memory_mb) / 1024)} GiB` : null
  const limit = kind.limit_mb !== undefined ? `${Math.round(Number(kind.limit_mb) / 1024)} GiB` : null
  if (peak && limit) {
    return `The scheduler killed this task — peak memory ${peak} exceeded the ${limit} cap. Rerun on a larger resource class.`
  }
  if (peak) {
    return `The scheduler killed this task for exceeding its memory cap (peak ${peak}). Rerun on a larger resource class.`
  }
  return 'The scheduler killed this task for exceeding its memory cap. Rerun on a larger resource class.'
}

function replayCorruptionSummary(kind: BlockerKind): string {
  if (kind.kind !== 'replay_corruption') return ''
  const versionLabel = kind.schema_version > 0 ? `v${kind.schema_version}` : 'unknown version'
  return `A persisted record (${kind.event_id}, ${versionLabel}) couldn't be loaded — your session is missing a slice of its history. Reason: ${kind.reason}.`
}

function imageDigestUnresolvedSummary(kind: BlockerKind): string {
  if (kind.kind !== 'image_digest_unresolved') return ''
  return `Couldn't pin a digest for ${kind.image}:${kind.tag}. Reason: ${kind.reason}.`
}

function sandboxRefusedSummary(kind: BlockerKind): string {
  if (kind.kind !== 'sandbox_refused') return ''
  if (kind.refusals.length === 0) {
    return 'The Phase 14 sandbox refused this task before dispatch.'
  }
  if (kind.refusals.length === 1) {
    const r = kind.refusals[0]
    return r!.detail
      ? `The Phase 14 sandbox refused this task before dispatch: ${r!.kind} — ${r!.detail}.`
      : `The Phase 14 sandbox refused this task before dispatch: ${r!.kind}.`
  }
  return `The Phase 14 sandbox refused this task before dispatch (${kind.refusals.length} reasons; see list below).`
}

/// Atom-safety-policy one-line summary for the
/// `sandbox_required` dispatch-time gate. The harness refused dispatch
/// because the atom's `safety.sandbox` requirement exceeds what the
/// chosen executor offers.
function sandboxRequiredSummary(kind: BlockerKind): string {
  if (kind.kind !== 'sandbox_required') return ''
  return `Atom ${kind.atom_id} needs the ${kind.requested} sandbox level, but the active executor only offers ${kind.available}.`
}

/// Atom-safety-policy one-line summary for the
/// `network_policy_mismatch` dispatch-time gate. Surfaces the atom's
/// declared NetworkPolicy alongside the executor's so the SME can
/// reconcile.
function networkPolicyMismatchSummary(kind: BlockerKind): string {
  if (kind.kind !== 'network_policy_mismatch') return ''
  return `Atom ${kind.atom_id} declares ${networkPolicyLabel(kind.atom_network)} but the executor enforces ${networkPolicyLabel(kind.executor_network)}.`
}

/// Atom-safety-policy one-line summary for the
/// `provisioning_denied` dispatch-time gate. The atom asked for a
/// runtime package that the dispatch policy hasn't whitelisted.
function provisioningDeniedSummary(kind: BlockerKind): string {
  if (kind.kind !== 'provisioning_denied') return ''
  return `Atom ${kind.atom_id} asked for ${kind.package} from the ${kind.registry} registry; the dispatch policy denied the request.`
}

/// Compact label for a typed NetworkPolicy. `bridge` is the historical
/// default. `none` carries an optional allowlist; render the count when
/// non-empty so the SME knows whether a per-host carve-out exists.
function networkPolicyLabel(policy: { kind: string; allowlist?: Array<string> }): string {
  if (policy.kind === 'bridge') return 'bridge (inherit harness network)'
  if (policy.kind === 'none') {
    const list = policy.allowlist ?? []
    if (list.length === 0) return 'none (no network)'
    return `none + allowlist of ${list.length} host${list.length === 1 ? '' : 's'}`
  }
  return policy.kind
}

/// V4 categorical axis grouping the 12 `SandboxRefusal` kinds.
/// Mirrors `crates/core/src/sandbox_refusal_category.rs::SandboxRefusalCategory`.
/// The seven categories drive recovery-hint dispatch in the BlockerCard
/// instead of a 12-arm per-kind switch.
type SandboxRefusalCategory =
  | 'network'
  | 'filesystem'
  | 'resource'
  | 'identity'
  | 'capability'
  | 'supply_chain'
  | 'output_validation'

/// TODO v4 P4 wire-up — v4 P4 will widen `RefusalKind::SandboxRefused`
/// with a typed `category: SandboxRefusalCategory` field populated by
/// `SandboxRefusal::category()` on the Rust side, at which point this
/// helper becomes `kind.category` read-through. Until then, project
/// from the stringified `kind_str` discriminator carried in
/// `SandboxRefusalRecord.kind`. Mirrors the match arm in
/// `crates/core/src/sandbox_policy.rs::SandboxRefusal::category()`
/// — keep in sync.
function categoryFromKind(refusalKind: string): SandboxRefusalCategory {
  switch (refusalKind) {
    case 'NetworkDenied':
      return 'network'
    case 'HostFsDenied':
      return 'filesystem'
    case 'MemoryLimitExceeded':
    case 'WallTimeoutExceeded':
      return 'resource'
    case 'SecretsDenied':
    case 'HumanReviewRequired':
    case 'ReviewStatusRejected':
      return 'identity'
    case 'StaticAnalysisRequired':
      return 'capability'
    case 'DependencyNotAllowed':
    case 'ContainerRequired':
    case 'SignedArtifactRequired':
      return 'supply_chain'
    case 'OutputSchemaValidationFailed':
      return 'output_validation'
    default:
      // Conservative fallback — unknown kinds surface under the
      // capability bucket (same as static-analysis, the closest
      // catch-all "needs an upstream gate to pass" category).
      return 'capability'
  }
}

/// One-line recovery hint per `SandboxRefusalCategory`. Replaces a
/// per-kind 12-arm string table with a 7-arm category switch, so adding
/// a new refusal kind on the Rust side only requires picking a category,
/// not authoring fresh UI copy. Drives the per-category footer in the
/// "Reasons the sandbox refused dispatch" section.
function recoveryHintForCategory(category: SandboxRefusalCategory): string {
  switch (category) {
    case 'network':
      return 'Network access is denied by the active sandbox policy. Either grant the task network egress in the policy bundle, or pivot to a method that runs offline.'
    case 'filesystem':
      return "Host filesystem access outside the task's workdir is denied. Stage inputs into the workdir or amend the policy to allow the requested path."
    case 'resource':
      return 'The task exceeded its resource cap. Rerun on a larger resource class or raise the per-task memory/wallclock limit in the policy bundle.'
    case 'identity':
      return 'The task needs an identity-axis gate to pass — secrets mount, human review, or non-Rejected review status. Have a reviewer sign off, or amend the stage to a method that does not require secrets.'
    case 'capability':
      return 'A required capability gate (e.g. static analysis) has not yet run or has not yet passed. Run the gate, fix any findings, and rerun.'
    case 'supply_chain':
      return 'A supply-chain requirement is unmet — dependency allowlist, container image, or signed-artifact pin. Update the policy bundle to permit the dependency, build the missing container, or sign the artifact.'
    case 'output_validation':
      return "The task's output failed schema validation against its declared postconditions. Fix the output shape or amend the stage's postcondition to match the actual output."
  }
}

function timeExceededSummary(kind: BlockerKind): string {
  if (kind.kind !== 'time_exceeded') return ''
  const elapsed = kind.wallclock_secs !== undefined ? `${Math.round(Number(kind.wallclock_secs) / 60)} minutes` : null
  const limit = kind.time_limit_secs !== undefined ? `${Math.round(Number(kind.time_limit_secs) / 60)} minutes` : null
  if (elapsed && limit) {
    return `The scheduler killed this task — elapsed time ${elapsed} reached the ${limit} wallclock cap. Rerun with a longer time limit.`
  }
  if (limit) {
    return `The scheduler killed this task for exceeding its ${limit} wallclock cap. Rerun with a longer time limit.`
  }
  return 'The scheduler killed this task for exceeding its wallclock cap. Rerun with a longer time limit.'
}

function buttonLabelFor(
  kind: BlockerKind | null | undefined,
  isDiscovery: boolean,
  busy: boolean,
): string {
  if (busy) return 'Working…'
  if (isDiscovery) return 'Accept selection and continue'
  if (!kind) return "I've addressed this — continue"
  switch (kind.kind) {
    case 'awaiting_sme_selection':
      return 'Continue once selection is made'
    case 'metric_below_threshold':
    case 'validation_failed':
      return 'Accept and continue'
    case 'data_shape_mismatch':
    case 'missing_input':
    case 'agent_error':
    case 'host_error':
    case 'pilot_oversize':
    case 'stalled':
    case 'contract_violation':
    case 'missing_artifact':
    case 'heartbeat_stalled':
    case 'orphaned_by_crash':
    case 'image_digest_mismatch':
    case 'container_pull_failed':
    case 'container_start_failed':
    case 'runtime_missing':
    case 'sbom_emission_failed':
    case 'network_policy_violation':
    case 'container_cache_corrupted':
      return "I've addressed this — continue"
    case 'container_hung':
      // Recovery is "reap container only, host stays" —
      // wired through the same Rerun-on-same-host path the orphan
      // reaper exposes; the card's generic button kicks the rerun.
      return 'Reap container & rerun'
    case 'iteration_did_not_converge':
      // Three-button picker (raise threshold / accept
      // best so far / abort) is the UX target; the generic button
      // is a fallback to "accept best" when the SME wants to move on.
      return 'Accept best iteration'
    case 'memory_exhausted':
      return 'Rerun with more memory'
    case 'time_exceeded':
      return 'Rerun with longer time limit'
    case 'replay_corruption':
      return 'Acknowledge — continue with partial history'
    case 'image_digest_unresolved':
      return 'Retry digest resolution'
    case 'composition_infeasible':
      return 'Open composer recovery'
    case 'container_exited_abnormally':
      return kind.oom_killed ? 'Rerun with more memory' : "I've addressed this — continue"
    case 'slurm_runtime_unavailable':
      return 'Pick a different partition'
    case 'runtime_capability_missing':
    case 'awaiting_structured_decision':
    case 'awaiting_sme_approval':
      // These variants route through the structured-decision path,
      // which ships its own Submit button; the generic label is a
      // safety fallback when no picker renders (e.g. blocker.json
      // missing on disk).
      return 'Submit decision'
    case 'tool_error':
      // tool_error renders the RemediationSuggestionList, which has
      // its own per-suggestion Apply buttons. The card's generic
      // bottom button is a safety fallback for "the SME wants to
      // bypass the proposer".
      return 'Skip suggestions and continue'
    case 'sandbox_refused':
      // Recovery is "amend the policy bundle, request
      // human review, or pivot to a non-generated method". The
      // generic button label kicks the same Unblock flow; the
      // detailed picker lives in the body.
      return 'Acknowledge — continue'
    case 'adjudication_required':
      // Generic button label routes through the same Unblock flow
      // until the dedicated adjudication picker lands.
      return 'Open adjudication queue'
    case 'sandbox_required':
    case 'network_policy_mismatch':
    case 'provisioning_denied':
      // Atom-safety-policy each variant ships dedicated
      // affordance buttons (Switch executor / Amend atom safety / Add
      // package) in its detail panel. The bottom generic button is a
      // safety fallback for SMEs who want to proceed without fixing
      // the policy via the dedicated affordance — fires the standard
      // Unblock flow so the harness retries dispatch.
      return "I've addressed this — continue"
    case 'schema_version_mismatch':
      // Config-manifest layout drift. Resolution is either to
      // upgrade the loader (rebuild + relaunch) or migrate the
      // on-disk manifest. The generic button label routes through
      // the standard Unblock flow once the operator has done one
      // of those out-of-band.
      return "I've migrated — retry load"
    case 'controlled_access_violation':
    case 'output_size_exceeded':
    case 'patch_unparseable':
    case 'clock_skew':
    case 'wall_clock_exceeded':
    case 'cancelled_by_amendment':
    case 'provenance_commit_dropped':
      return "I've addressed this — continue"
    case 'turn_budget_exceeded':
      return 'Investigate'
  }
  // Exhaustiveness: adding a new BlockerKind variant forces an explicit
  // button-label decision here; `tsc --noEmit` fails otherwise.
  const _exhaustive: never = kind
  void _exhaustive
  return "I've addressed this — continue"
}

/// Heuristic: parse the blocker reason string for the
/// "runtime/outputs/<task_id>/decision.json" tail the agent writes
/// into its reason (see scripts/agent-claude.sh discovery-default-block
/// prompt). Returns null if the reason doesn't reference a decision
/// file.
function extractDecisionTaskId(reason: string): string | null {
  const m = reason.match(
    /runtime\/outputs\/([A-Za-z0-9_.-]+)\/decision\.json/,
  )
  return m ? m[1]! : null
}

/// Extract a task id from a blocker.json reference in the reason text.
/// Structured-decision blockers reference this file (`runtime/outputs/<task>/blocker.json`)
/// rather than decision.json. Same regex shape, different filename.
function extractBlockerTaskId(reason: string): string | null {
  const m = reason.match(
    /runtime\/outputs\/([A-Za-z0-9_.-]+)\/blocker\.json/,
  )
  return m ? m[1]! : null
}

/// Decide whether the blocker kind routes to the structured-decision
/// picker. Routes when:
/// - blockerKind.kind is one of the three new structured variants, OR
/// - reason text references a blocker.json artifact (belt-and-braces
/// for sessions whose BlockerKind is still a legacy
/// DataShapeMismatch fallback from a pre-mapper server)
function isStructuredDecisionKind(
  kind: BlockerKind | null | undefined,
  reason: string,
): boolean {
  if (kind) {
    if (
      kind.kind === 'awaiting_structured_decision' ||
      kind.kind === 'runtime_capability_missing' ||
      kind.kind === 'awaiting_sme_approval'
    ) {
      return true
    }
  }
  return extractBlockerTaskId(reason) !== null
}

export default function BlockerCard({
  reason,
  recoveryHint,
  onUnblock,
  disabled,
  blockerKind,
  sessionId,
  relatedDispositionPath,
  relatedDispositionTaskId,
  taskId: explicitTaskId,
}: Props) {
  const taskId = explicitTaskId ?? extractDecisionTaskId(reason)
  // Structured-decision path: prefer the explicit prop (the caller
  // already knows the task), fall back to extracting from the reason
  // text or the typed kind.
  const structuredTaskId = useMemo(() => {
    if (explicitTaskId) return explicitTaskId
    const fromReason = extractBlockerTaskId(reason)
    if (fromReason) return fromReason
    if (blockerKind?.kind === 'awaiting_structured_decision') {
      return blockerKind.task_id
    }
    return null
  }, [reason, blockerKind, explicitTaskId])
  // When the caller supplies an explicit taskId we always treat the
  // card as structured — the picker fetches blocker.json for that
  // task and renders its decision_points_for_sme (or the empty-state
  // placeholder if none exist yet).
  const isStructured = useMemo(
    () => Boolean(explicitTaskId) || isStructuredDecisionKind(blockerKind, reason),
    [blockerKind, reason, explicitTaskId],
  )
  const [decision, setDecision] = useState<DiscoveryDecision | null>(null)
  const [chosen, setChosen] = useState<string | null>(null)
  const [autoApproveAll, setAutoApproveAll] = useState(false)
  const { busy, error: err, run, clearError: _clearError } = useAsync()
  const [fetchErr, setFetchErr] = useState<string | null>(null)
  // Structured-decision state: agent-written blocker.json + the SME's
  // per-decision_point selections.
  const [structured, setStructured] = useState<AgentBlockerJson | null>(null)
  const [answers, setAnswers] = useState<Record<string, string>>({})
  const [structuredRationale, setStructuredRationale] = useState('')
  const [attempts, setAttempts] = useState<BlockerAttempt[]>([])
  const [rerunBusy, setRerunBusy] = useState<string | null>(null)
  const [rerunMessage, setRerunMessage] = useState<string | null>(null)

  // Fetch decision.json once on mount when we have a discovery-tagged
  // reason + sessionId. Set the default chosen to the top candidate so
  // a click-through-Unblock ratifies the agent's pick.
  //
  // Cohort-coverage override: if blocker.json (more recent than
  // decision.json) carries a `top_candidate_components` array, the
  // agent has flagged decision.top_candidate as having partial
  // accession coverage and recommended a hybrid. Decompose the hybrid:
  // pick the first component whose method_id is NOT a local-path
  // family ("sme_supplied_local_path" / "sme_supplied_uploaded_files",
  // both auto-honored by the agent regardless of `chosen`). The
  // fetcher half is what needs explicit selection. This closes the
  // "decision.json picked sme_supplied_local_path with 15% coverage,
  // BlockerCard rubber-stamped it, agent dropped 10/12 studies"
  // regression observed in Run #7.
  useCancelableEffect(async ({ cancelled }) => {
    if (!taskId || !sessionId) return
    // decision.json is a discovery-task artifact. Skip the fetch for
    // non-discover tasks — it produces a console-noisy 404 storm on
    // any compute-task blocker (e.g. TurnBudgetExceeded on normalisation)
    // and the decision picker has nothing to render anyway.
    if (!taskId.startsWith('discover_')) return
    try {
      const d = await getTaskDecision(sessionId, taskId)
      if (cancelled()) return
      if (!d) return
      setDecision(d)
      // Sequential (not Promise.all) so mock-fetch ordering in tests
      // stays deterministic. blocker.json is best-effort: 404s and
      // network blips fall through to the decision-only default.
      let initialChosen = d.top_candidate
      try {
        const b = await getTaskBlocker(sessionId, taskId)
        if (
          !cancelled() &&
          b &&
          Array.isArray(b.top_candidate_components) &&
          b.top_candidate_components.length > 1
        ) {
          const fetcher = b.top_candidate_components.find(
            (c) =>
              c.method_id !== 'sme_supplied_local_path' &&
              c.method_id !== 'sme_supplied_uploaded_files',
          )
          if (fetcher && fetcher.method_id) {
            initialChosen = fetcher.method_id
          }
        }
      } catch {
        /* blocker.json missing — keep decision.top_candidate */
      }
      if (!cancelled()) setChosen(initialChosen)
    } catch (e) {
      if (!cancelled()) setFetchErr((e as Error).message)
    }
  }, [taskId, sessionId])

  // Structured-decision fetch: mirrors the discovery path but points
  // at runtime/outputs/<task>/blocker.json. Pre-populates `answers`
  // with default_if_unanswered so a one-click "Accept defaults" flow
  // works out of the box. Also captures the attempts list (read from
  // WORKFLOW.json server-side and merged into the blocker response)
  // for the "Tried so far" history rendered below the picker.
  useCancelableEffect(async ({ cancelled }) => {
    if (!isStructured || !structuredTaskId || !sessionId) return
    try {
      const payload = await getTaskBlockerPayload(sessionId, structuredTaskId)
      if (cancelled()) return
      const b = payload.blocker as AgentBlockerJson | null
      if (b) {
        setStructured(b)
        const defaults: Record<string, string> = {}
        for (const dp of b.decision_points_for_sme ?? []) {
          if (dp.default_if_unanswered) {
            defaults[dp.id] = dp.default_if_unanswered
          } else if (dp.options && dp.options.length > 0) {
            defaults[dp.id] = dp!.options[0]!.id
          }
        }
        setAnswers(defaults)
      }
      setAttempts(payload.attempts ?? [])
    } catch (e) {
      if (!cancelled()) setFetchErr((e as Error).message)
    }
  }, [isStructured, structuredTaskId, sessionId])

  const handleRerunScript = async () => {
    if (!sessionId || !structuredTaskId || !structured?.recoverable_action) return
    const action = structured.recoverable_action
    setRerunBusy(action.rel_path)
    setRerunMessage(null)
    try {
      const res = await postRerunScript(
        sessionId,
        structuredTaskId,
        action.rel_path,
      )
      setRerunMessage(
        res.pid != null
          ? `Re-running ${action.rel_path} (pid ${res.pid}). Watch the Logs tab for the new output.`
          : `Re-launch requested for ${action.rel_path}: ${res.message}`,
      )
    } catch (e) {
      setRerunMessage(`Re-run failed: ${(e as Error).message}`)
    } finally {
      setRerunBusy(null)
    }
  }

  const isDiscovery = !!decision
  const title = titleFor(blockerKind, isDiscovery)

  const handleAccept = async () => {
    await run(async () => {
      // 1. Persist the SME's choice to the package so the resuming
      // agent uses it. Must fire for BOTH top-candidate and runner-up
      // picks: the agent's approval-detection logic in
      // scripts/agent-claude.sh only advances when it finds
      // sme-selection.json (or sme-decisions.json / a CONTEXT.md SME
      // runtime decisions section). Without the file the agent
      // re-scores → re-blocks identically because the deny-list gate
      // has no SME input to consume.
      if (decision && chosen && sessionId && taskId) {
        await postSmeSelection(sessionId, taskId, chosen)
      }
      // 2. If SME opted into auto-approve for all future discoveries,
      // flip the session-scoped marker BEFORE unblocking so the next
      // iteration already sees the flag.
      if (autoApproveAll && sessionId) {
        await setAutoApproveDiscoveries(sessionId)
      }
      // 3. Unblock the session. The existing /unblock handler flips
      // the blocked task back to Ready so the harness resumes.
      await onUnblock()
    })
  }

  // Structured-decision submit: POST sme-decisions.json (fires
  // auto-relaunch server-side via maybe_auto_relaunch_harness) and
  // then fall through to unblock(). Missing answers or missing
  // session/task id short-circuit to inline errors; users can always
  // still click "Unblock" to force a resume without answers.
  const handleStructuredSubmit = async () => {
    await run(async () => {
      if (!sessionId || !structuredTaskId) {
        throw new Error('session + task id required')
      }
      const dps = structured?.decision_points_for_sme ?? []
      // Empty-decision-points path: the agent hasn't recorded any
      // structured questions for this blocker (e.g. an explicit-taskId
      // render of a `missing_artifact` blocker, or a structured blocker
      // whose blocker.json arrived without dps). The empty-state copy
      // promises a clean unblock with no recorded answer, so skip the
      // POST and fall straight through to onUnblock — matches the
      // discovery-less branch of handleAccept.
      if (dps.length > 0) {
        const payload = dps
          .map((dp) => ({
            id: dp.id,
            chosen: answers[dp.id],
          }))
          .filter(
            (a): a is { id: string; chosen: string } =>
              typeof a.chosen === 'string' && a.chosen.length > 0,
          )
        if (payload.length === 0) {
          throw new Error('no decisions chosen — pick at least one option')
        }
        await postSmeDecisions(
          sessionId,
          structuredTaskId,
          payload,
          structuredRationale.trim() || undefined,
        )
      }
      await onUnblock()
    })
  }

  const buttonLabel = buttonLabelFor(blockerKind, isDiscovery, busy)
  const candidates: string[] = decision
    ? [
        decision.top_candidate,
        ...(decision.runner_ups ?? []).filter(
          (c) => c !== decision.top_candidate,
        ),
      ]
    : []

  return (
    <section
      role="alert"
      aria-label={`Conversation blocked — ${title}`}
      data-blocker-kind={blockerKind?.kind ?? 'generic'}
      data-discovery-approval={isDiscovery ? 'true' : 'false'}
      style={{
        display: 'flex',
        flexDirection: 'column',
        maxHeight: 'min(80vh, 720px)',
        marginTop: '0.75rem',
        padding: '0.85rem 1rem',
        background: 'var(--color-warning-bg)',
        border: '1px solid #fcd34d',
        borderLeft: '4px solid #d97706',
        borderRadius: 8,
      }}
    >
      <strong
        style={{
          display: 'block',
          fontSize: '0.85rem',
          color: 'var(--color-warning-fg)',
          marginBottom: 6,
        }}
      >
        {title}
      </strong>

      {relatedDispositionPath && (
        <p
          role="note"
          data-testid="related-disposition-hint"
          style={{
            marginTop: 0,
            marginBottom: '0.5rem',
            padding: '0.4rem 0.6rem',
            fontSize: '0.76rem',
            color: 'var(--color-info-fg, #1d4ed8)',
            background: 'var(--color-info-bg, #eff6ff)',
            border: '1px solid var(--color-info-border, #bfdbfe)',
            borderRadius: 4,
            lineHeight: 1.45,
          }}
        >
          ⓘ An unapplied disposition
          {relatedDispositionTaskId
            ? ` on ${relatedDispositionTaskId}`
            : ''}{' '}
          may resolve this — review it below before unblocking.
        </p>
      )}

      <div
        data-testid="blocker-card-body"
        style={{
          flex: '1 1 auto',
          minHeight: 0,
          overflowY: 'auto',
          marginRight: '-0.5rem',
          paddingRight: '0.5rem',
        }}
      >
      {isStructured && !isDiscovery ? (
        <StructuredDecisionPicker
          blocker={structured}
          answers={answers}
          onAnswerChange={(id, value) =>
            setAnswers((prev) => ({ ...prev, [id]: value }))
          }
          rationale={structuredRationale}
          onRationaleChange={setStructuredRationale}
          reason={reason}
          blockerKind={blockerKind}
        />
      ) : isDiscovery ? (
        <>
          <p
            style={{
              margin: 0,
              fontSize: '0.82rem',
              color: 'var(--color-warning-fg)',
              lineHeight: 1.5,
            }}
          >
            The agent ran best-practice scoring and wants your approval
            before committing to a method. Review the candidates and
            accept the agent's top pick (recommended) or override to a
            runner-up.
          </p>
          {decision?.rationale && (
            <p
              style={{
                marginTop: '0.4rem',
                fontSize: '0.76rem',
                color: 'var(--color-warning-fg)',
                fontStyle: 'italic',
              }}
            >
              {decision.rationale}
            </p>
          )}
          <fieldset
            aria-label={`Candidates for ${taskId}`}
            style={{
              marginTop: '0.6rem',
              marginBottom: '0.6rem',
              padding: '0.5rem 0.75rem',
              border: '1px solid #fcd34d',
              borderRadius: 6,
              background: 'var(--color-warning-bg)',
            }}
          >
            <legend
              style={{
                padding: '0 0.35rem',
                fontSize: '0.72rem',
                color: 'var(--color-warning-fg)',
                fontWeight: 600,
              }}
            >
              {taskId}
            </legend>
            <RadioRow<string>
              name={`candidate-${taskId}`}
              value={chosen}
              onChange={setChosen}
              options={candidates.map<RadioOption<string>>((c, i) => {
                const score = decision?.scores?.[c]
                const isTop = c === decision?.top_candidate
                return {
                  value: c,
                  ariaLabel: c,
                  label: (
                    <>
                      <span
                        style={{
                          fontFamily: 'ui-monospace, monospace',
                          fontWeight: isTop ? 700 : 400,
                        }}
                      >
                        {c}
                      </span>
                      {isTop && (
                        <span
                          aria-label="Agent's recommended candidate"
                          style={{
                            fontSize: '0.65rem',
                            padding: '1px 6px',
                            background: 'var(--color-warning-accent)',
                            color: 'var(--color-text-on-accent)',
                            borderRadius: 3,
                            fontWeight: 600,
                          }}
                        >
                          ★ RECOMMENDED
                        </span>
                      )}
                      {typeof score === 'number' && (
                        <span
                          style={{
                            marginLeft: 'auto',
                            fontFamily: 'ui-monospace, monospace',
                            fontSize: '0.76rem',
                            color: 'var(--color-warning-fg)',
                          }}
                          title="0–1, higher is better"
                        >
                          score {score.toFixed(2)}/1.0
                        </span>
                      )}
                      {i === 0 && candidates.length === 1 && (
                        <span
                          style={{
                            marginLeft: 'auto',
                            fontSize: '0.72rem',
                            color: 'var(--color-warning-fg)',
                            fontStyle: 'italic',
                          }}
                        >
                          (only candidate)
                        </span>
                      )}
                    </>
                  ),
                }
              })}
            />
          </fieldset>
          <label
            style={{
              display: 'flex',
              alignItems: 'center',
              gap: '0.4rem',
              fontSize: '0.76rem',
              color: 'var(--color-warning-fg)',
              marginBottom: '0.6rem',
              cursor: 'pointer',
            }}
          >
              <input
              type="checkbox"
              checked={autoApproveAll}
              onChange={(e) => setAutoApproveAll(e.target.checked)}
              aria-label="Auto-approve routine discoveries (not integration, annotation, DE, or validation)"
            />
            Auto-approve routine discoveries (not integration, annotation, DE, or validation)
          </label>
        </>
      ) : (
        <>
          <p
            style={{
              margin: 0,
              fontSize: '0.83rem',
              color: 'var(--color-warning-fg)',
              lineHeight: 1.5,
            }}
          >
            {blockerKind?.kind === 'stalled'
              ? stallSignalSummary(blockerKind)
              : blockerKind?.kind === 'pilot_oversize'
                ? pilotOversizeSummary(blockerKind)
                : blockerKind?.kind === 'missing_artifact'
                  ? missingArtifactSummary(blockerKind)
                  : blockerKind?.kind === 'heartbeat_stalled'
                    ? heartbeatStalledSummary(blockerKind)
                    : blockerKind?.kind === 'orphaned_by_crash'
                      ? orphanedByCrashSummary(blockerKind)
                      : blockerKind?.kind === 'memory_exhausted'
                        ? memoryExhaustedSummary(blockerKind)
                        : blockerKind?.kind === 'time_exceeded'
                          ? timeExceededSummary(blockerKind)
                          : blockerKind?.kind === 'replay_corruption'
                            ? replayCorruptionSummary(blockerKind)
                            : blockerKind?.kind === 'image_digest_unresolved'
                              ? imageDigestUnresolvedSummary(blockerKind)
                              : blockerKind?.kind === 'sandbox_refused'
                                ? sandboxRefusedSummary(blockerKind)
                                : blockerKind?.kind === 'sandbox_required'
                                  ? sandboxRequiredSummary(blockerKind)
                                  : blockerKind?.kind === 'network_policy_mismatch'
                                    ? networkPolicyMismatchSummary(blockerKind)
                                    : blockerKind?.kind === 'provisioning_denied'
                                      ? provisioningDeniedSummary(blockerKind)
                                      : sanitizeForSme(reason)}
            <ExplainButton text={reason} context="blocker reason" />
          </p>
          <p
            style={{
              marginTop: '0.5rem',
              marginBottom: '0.75rem',
              fontSize: '0.78rem',
              color: 'var(--color-warning-fg)',
              fontStyle: 'italic',
            }}
          >
            {sanitizeForSme(recoveryHint)}
          </p>
        </>
      )}
      {attempts.length > 0 && (
        <AttemptsHistory attempts={attempts} />
      )}
      {blockerKind?.kind === 'tool_error' && sessionId && (explicitTaskId || blockerKind.envelope.task_id) && (
        <RemediationSuggestionList
          sessionId={sessionId}
          taskId={explicitTaskId ?? blockerKind.envelope.task_id}
          onApplied={() => {
            // After a remediation applies, fire the standard Unblock
            // flow so the parent (TaskDetailDrawer / chat surface)
            // refreshes session state.
            void run(() => Promise.resolve(onUnblock()))
          }}
        />
      )}
      {blockerKind?.kind === 'composition_infeasible' && (
        <CompositionInfeasibleCard blockerKind={blockerKind} />
      )}
      {blockerKind?.kind === 'sandbox_refused' && blockerKind.refusals.length > 0 && (
        <SandboxRefusalSection refusals={blockerKind.refusals} />
      )}
      {blockerKind?.kind === 'sandbox_required' && (
        <SandboxRequiredSection blockerKind={blockerKind} />
      )}
      {blockerKind?.kind === 'network_policy_mismatch' && (
        <NetworkPolicyMismatchSection blockerKind={blockerKind} />
      )}
      {blockerKind?.kind === 'provisioning_denied' && (
        <ProvisioningDeniedSection
          blockerKind={blockerKind}
          sessionId={sessionId ?? null}
        />
      )}
      </div>
      {structured?.recoverable_action?.kind === 'rerun_script' && (
        <RerunScriptStrip
          action={structured.recoverable_action}
          busy={rerunBusy}
          message={rerunMessage}
          onClick={handleRerunScript}
        />
      )}
      {(err ?? fetchErr) && (
        <p
          role="alert"
          style={{
            marginBottom: '0.5rem',
            fontSize: '0.76rem',
            color: 'var(--color-danger-fg)',
          }}
        >
          {err ?? fetchErr}
        </p>
      )}
      {blockerKind?.kind === 'stalled' ? (
        <StallActionButtons
          defaultAction={blockerKind.suggested_action as StallResolution}
          disabled={disabled || busy}
          busy={busy}
          onResolve={async (resolution) => {
            await run(() => Promise.resolve(onUnblock(resolution)))
          }}
        />
      ) : isStructured && !isDiscovery ? (
        <button
          type="button"
          onClick={handleStructuredSubmit}
          disabled={disabled || busy}
          style={{
            padding: '0.45rem 0.9rem',
            background: 'var(--color-warning-accent)',
            color: 'var(--color-text-on-accent)',
            border: 'none',
            borderRadius: 6,
            cursor: disabled || busy ? 'not-allowed' : 'pointer',
            fontSize: '0.8rem',
            fontWeight: 600,
            opacity: disabled || busy ? 0.6 : 1,
          }}
        >
          {busy ? 'Submitting…' : 'Apply decision and continue'}
        </button>
      ) : (
        <button
          type="button"
          onClick={handleAccept}
          disabled={disabled || busy}
          style={{
            padding: '0.45rem 0.9rem',
            background: 'var(--color-warning-accent)',
            color: 'var(--color-text-on-accent)',
            border: 'none',
            borderRadius: 6,
            cursor: disabled || busy ? 'not-allowed' : 'pointer',
            fontSize: '0.8rem',
            fontWeight: 600,
            opacity: disabled || busy ? 0.6 : 1,
          }}
        >
          {buttonLabel}
        </button>
      )}
    </section>
  )
}

/// Render the agent's structured decision_points_for_sme picker.
/// Each decision_point becomes a labelled fieldset with radio options
/// (label + risk + consequence). `default_if_unanswered`, if set, is
/// pre-selected so the SME can click Apply to accept the agent's
/// default without reading every option. A single rationale textarea
/// at the bottom captures the SME's overall reasoning.
function StructuredDecisionPicker({
  blocker,
  answers,
  onAnswerChange,
  rationale,
  onRationaleChange,
  reason,
  blockerKind,
}: {
  blocker: AgentBlockerJson | null
  answers: Record<string, string>
  onAnswerChange: (id: string, value: string) => void
  rationale: string
  onRationaleChange: (value: string) => void
  reason: string
  blockerKind?: BlockerKind | null
}): JSX.Element {
  const dps = blocker?.decision_points_for_sme ?? []

  // Top-level explanation — uses typed fields when the blocker kind
  // carries them (RuntimeCapabilityMissing prints method + capability +
  // substitute), otherwise falls back to the sanitized reason prose.
  const intro = (() => {
    if (blockerKind?.kind === 'runtime_capability_missing') {
      const sub = blockerKind.recommended_substitute
      return (
        <>
          <strong>{blockerKind.sme_pinned_method}</strong> needs{' '}
          <code
            style={{
              fontFamily: 'ui-monospace, monospace',
              background: 'var(--color-warning-bg)',
              padding: '0 4px',
              borderRadius: 3,
            }}
          >
            {blockerKind.missing_capability}
          </code>
          , which isn't installed in this environment.
          {sub && (
            <>
              {' '}
              The agent suggests{' '}
              <code
                style={{
                  fontFamily: 'ui-monospace, monospace',
                  background: 'var(--color-warning-bg)',
                  padding: '0 4px',
                  borderRadius: 3,
                }}
              >
                {sub}
              </code>{' '}
              as a same-algorithm substitute.
            </>
          )}
        </>
      )
    }
    return sanitizeForSme(blocker?.summary ?? reason)
  })()

  return (
    <>
      <p
        style={{
          margin: 0,
          fontSize: '0.82rem',
          color: 'var(--color-warning-fg)',
          lineHeight: 1.5,
        }}
      >
        {intro}
      </p>

      {dps.length === 0 ? (
        <p
          data-testid="structured-blocker-empty"
          style={{
            marginTop: '0.5rem',
            fontSize: '0.78rem',
            color: 'var(--color-warning-fg)',
            fontStyle: 'italic',
          }}
        >
          The agent hasn't recorded specific decision points yet. Click
          "Apply decision and continue" to unblock with no answer, or
          review the activity log for context.
        </p>
      ) : (
        dps.map((dp) => {
          const options = dp.options ?? []
          const currentAnswer = answers[dp.id] ?? ''
          const defaultId = dp.default_if_unanswered ?? null
          return (
            <fieldset
              key={dp.id}
              data-decision-point-id={dp.id}
              aria-label={dp.question}
              style={{
                marginTop: '0.7rem',
                padding: '0.6rem 0.8rem',
                border: '1px solid #fcd34d',
                borderRadius: 6,
                background: 'var(--color-warning-bg)',
              }}
            >
              <legend
                style={{
                  padding: '0 0.4rem',
                  fontSize: '0.76rem',
                  color: 'var(--color-warning-fg)',
                  fontWeight: 600,
                }}
              >
                {dp.question}
              </legend>
              <div
                role="radiogroup"
                style={{
                  display: 'flex',
                  flexDirection: 'column',
                  gap: '0.35rem',
                  marginTop: '0.4rem',
                }}
              >
                {options.map((opt) => {
                  const isDefault = opt.id === defaultId
                  const checked = currentAnswer === opt.id
                  return (
                    <label
                      key={opt.id}
                      data-option-id={opt.id}
                      style={{
                        display: 'flex',
                        gap: '0.5rem',
                        fontSize: '0.78rem',
                        color: 'var(--color-warning-fg)',
                        cursor: 'pointer',
                        alignItems: 'flex-start',
                        padding: '0.3rem',
                        borderRadius: 4,
                        background: checked ? 'var(--color-warning-border)' : 'transparent',
                      }}
                    >
                      <input
                        type="radio"
                        name={`dp-${dp.id}`}
                        value={opt.id}
                        checked={checked}
                        onChange={() => onAnswerChange(dp.id, opt.id)}
                        style={{ marginTop: 2 }}
                      />
                      <span style={{ display: 'flex', flexDirection: 'column', gap: 2, flex: 1 }}>
                        <span>
                          <strong>{opt.label ?? opt.id}</strong>
                          {isDefault && (
                            <span
                              data-testid="default-option-badge"
                              style={{
                                marginLeft: 8,
                                fontSize: '0.65rem',
                                padding: '1px 6px',
                                background: 'var(--color-warning-accent)',
                                color: 'var(--color-text-on-accent)',
                                borderRadius: 3,
                                fontWeight: 600,
                              }}
                            >
                              AGENT DEFAULT
                            </span>
                          )}
                        </span>
                        {opt.risk && (
                          <span style={{ fontSize: '0.7rem', color: 'var(--color-warning-fg)' }}>
                            Risk: {opt.risk}
                          </span>
                        )}
                        {opt.consequence && (
                          <span style={{ fontSize: '0.7rem', color: 'var(--color-warning-fg)' }}>
                            If chosen: {opt.consequence}
                          </span>
                        )}
                      </span>
                    </label>
                  )
                })}
              </div>
            </fieldset>
          )
        })
      )}

      <label
        style={{
          display: 'block',
          marginTop: '0.7rem',
          fontSize: '0.74rem',
          color: 'var(--color-warning-fg)',
          fontWeight: 500,
        }}
      >
        Reason (optional, saved to the audit log)
        <textarea
          data-testid="structured-rationale"
          value={rationale}
          onChange={(e) => onRationaleChange(e.target.value)}
          rows={2}
          style={{
            width: '100%',
            marginTop: 4,
            padding: '0.4rem 0.5rem',
            borderRadius: 4,
            border: '1px solid #fcd34d',
            fontSize: '0.78rem',
            fontFamily: 'inherit',
            background: 'var(--color-warning-bg)',
            resize: 'vertical',
            boxSizing: 'border-box',
          }}
          placeholder="A short note on why you chose this — shows up in the Decisions tab."
        />
      </label>
    </>
  )
}

/// Three-button panel for `Stalled` blockers. `defaultAction` is
/// highlighted; all three actions are always offered.
function StallActionButtons({
  defaultAction,
  disabled,
  busy,
  onResolve,
}: {
  defaultAction: StallResolution
  disabled: boolean
  busy: boolean
  onResolve: (resolution: StallResolution) => void | Promise<void>
}): JSX.Element {
  const actions: Array<{ key: StallResolution; label: string; description: string }> = [
    {
      key: 'resize',
      label: 'Resize and resume',
      description: 'Provision a larger instance and retry the task.',
    },
    {
      key: 'retry',
      label: 'Retry',
      description: 'Rerun on the same instance shape.',
    },
    {
      key: 'abort',
      label: 'Abort task',
      description: 'Mark the task failed and stop retrying.',
    },
  ]
  return (
    <div
      role="group"
      aria-label="Stall recovery actions"
      style={{ display: 'flex', flexDirection: 'column', gap: '0.5rem' }}
    >
      <div style={{ display: 'flex', gap: '0.4rem', flexWrap: 'wrap' }}>
        {actions.map((a) => {
          const isDefault = a.key === defaultAction
          return (
            <button
              key={a.key}
              type="button"
              data-resolution={a.key}
              data-default-resolution={isDefault ? 'true' : 'false'}
              onClick={() => onResolve(a.key)}
              disabled={disabled}
              title={a.description}
              style={{
                padding: '0.45rem 0.9rem',
                background: isDefault ? 'var(--color-warning-accent)' : 'transparent',
                color: isDefault ? 'var(--color-surface-1)' : 'var(--color-warning-fg)',
                border: `1px solid ${isDefault ? 'var(--color-warning-accent)' : 'var(--color-warning-border)'}`,
                borderRadius: 6,
                cursor: disabled ? 'not-allowed' : 'pointer',
                fontSize: '0.78rem',
                fontWeight: 600,
                opacity: disabled ? 0.6 : 1,
              }}
            >
              {busy && isDefault ? 'Working…' : a.label}
            </button>
          )
        })}
      </div>
    </div>
  )
}

/// V4 sandbox-refused detail panel. Groups refusals by the
/// 7-element `SandboxRefusalCategory` axis instead of rendering a flat
/// 12-arm list, so the SME sees recovery hints scoped to "the network
/// policy is denying this" or "the supply-chain pin is unmet" instead
/// of a generic catch-all. Within each category the per-kind detail
/// strings are still rendered for diagnostics; the category header
/// drives the recovery copy. The fallback hint at the bottom is
/// retained for the multi-category case (when the SME has more than
/// one axis to clear before dispatch can proceed).
function SandboxRefusalSection({
  refusals,
}: {
  refusals: SandboxRefusalRecord[]
}): JSX.Element {
  // Group refusals by category in declaration order. Inserting into a
  // Map preserves first-seen order so the rendered category list is
  // deterministic across renders.
  const grouped = new Map<SandboxRefusalCategory, SandboxRefusalRecord[]>()
  for (const r of refusals) {
    const cat = categoryFromKind(r.kind)
    const bucket = grouped.get(cat)
    if (bucket) {
      bucket.push(r)
    } else {
      grouped.set(cat, [r])
    }
  }
  const categoryEntries = Array.from(grouped.entries())

  return (
    <section
      aria-label="Sandbox refusal details"
      data-testid="sandbox-refused-list"
      style={{
        marginTop: '0.6rem',
        padding: '0.5rem 0.75rem',
        border: '1px solid #fcd34d',
        borderRadius: 6,
        background: 'var(--color-warning-bg)',
      }}
    >
      <strong
        style={{
          display: 'block',
          fontSize: '0.78rem',
          color: 'var(--color-warning-fg)',
          marginBottom: 4,
        }}
      >
        Reasons the sandbox refused dispatch
      </strong>
      {categoryEntries.map(([category, items]) => (
        <div
          key={category}
          data-testid={`sandbox-refused-category-${category}`}
          data-sandbox-refusal-category={category}
          style={{ marginTop: '0.4rem' }}
        >
          <strong
            style={{
              display: 'block',
              fontSize: '0.74rem',
              color: 'var(--color-warning-fg)',
              textTransform: 'uppercase',
              letterSpacing: '0.04em',
            }}
          >
            {category.replace('_', ' ')}
          </strong>
          <ul style={{ margin: '0.2rem 0 0', paddingLeft: '1.2rem' }}>
            {items.map((r, i) => (
              <li
                key={`${category}-${i}`}
                style={{
                  fontSize: '0.78rem',
                  color: 'var(--color-warning-fg)',
                  lineHeight: 1.5,
                }}
              >
                <strong>{r.kind}</strong>
                {r.detail ? <> &mdash; {r.detail}</> : null}
              </li>
            ))}
          </ul>
          <p
            style={{
              marginTop: '0.25rem',
              marginBottom: 0,
              fontSize: '0.74rem',
              color: 'var(--color-warning-fg)',
              fontStyle: 'italic',
            }}
          >
            {recoveryHintForCategory(category)}
          </p>
        </div>
      ))}
      {categoryEntries.length > 1 && (
        <p
          style={{
            marginTop: '0.5rem',
            marginBottom: 0,
            fontSize: '0.74rem',
            color: 'var(--color-warning-fg)',
            fontStyle: 'italic',
          }}
        >
          Multiple refusal axes must clear before this task can dispatch.
          Address each category above, then unblock.
        </p>
      )}
    </section>
  )
}

/// Atom-safety-policy dedicated panel for the
/// `sandbox_required` dispatch-time gate. Surfaces the atom id +
/// requested-vs-available sandbox levels and offers a "Switch
/// executor" affordance. The button links to the Settings page where
/// the SME can pick an executor with the requested sandbox level —
/// the full chooser flow is out of scope for this task.
function SandboxRequiredSection({
  blockerKind,
}: {
  blockerKind: Extract<BlockerKind, { kind: 'sandbox_required' }>
}): JSX.Element {
  return (
    <section
      aria-label="Sandbox upgrade required"
      data-testid="sandbox-required-detail"
      style={{
        marginTop: '0.6rem',
        padding: '0.5rem 0.75rem',
        border: '1px solid #fcd34d',
        borderRadius: 6,
        background: 'var(--color-warning-bg)',
      }}
    >
      <dl
        style={{
          display: 'grid',
          gridTemplateColumns: 'auto 1fr',
          gap: '0.25rem 0.6rem',
          margin: 0,
          fontSize: '0.76rem',
          color: 'var(--color-warning-fg)',
        }}
      >
        <dt style={{ fontWeight: 600 }}>Atom</dt>
        <dd style={{ margin: 0, fontFamily: 'ui-monospace, monospace' }}>
          {blockerKind.atom_id}
        </dd>
        <dt style={{ fontWeight: 600 }}>Requested</dt>
        <dd style={{ margin: 0, fontFamily: 'ui-monospace, monospace' }}>
          {blockerKind.requested}
        </dd>
        <dt style={{ fontWeight: 600 }}>Available</dt>
        <dd style={{ margin: 0, fontFamily: 'ui-monospace, monospace' }}>
          {blockerKind.available}
        </dd>
      </dl>
      <div
        style={{
          marginTop: '0.5rem',
          display: 'flex',
          gap: '0.4rem',
          flexWrap: 'wrap',
        }}
      >
        <button
          type="button"
          onClick={() => {
            window.dispatchEvent(new CustomEvent('ecaax:open-settings'))
          }}
          data-testid="switch-executor-button-sandbox"
          style={{
            padding: '0.35rem 0.75rem',
            background: 'var(--color-warning-accent)',
            color: 'var(--color-text-on-accent)',
            border: 'none',
            borderRadius: 4,
            fontSize: '0.76rem',
            fontWeight: 600,
            cursor: 'pointer',
          }}
        >
          Switch executor
        </button>
      </div>
    </section>
  )
}

/// Atom-safety-policy dedicated panel for the
/// `network_policy_mismatch` dispatch-time gate. Renders the atom and
/// executor NetworkPolicies side-by-side and offers two affordances:
/// "Switch executor" (programmatic navigation to Settings) + "Amend
/// atom safety" (disabled placeholder until the amend-safety flow is
/// wired — see Task 4.7+).
function NetworkPolicyMismatchSection({
  blockerKind,
}: {
  blockerKind: Extract<BlockerKind, { kind: 'network_policy_mismatch' }>
}): JSX.Element {
  return (
    <section
      aria-label="Network policy mismatch"
      data-testid="network-policy-mismatch-detail"
      style={{
        marginTop: '0.6rem',
        padding: '0.5rem 0.75rem',
        border: '1px solid #fcd34d',
        borderRadius: 6,
        background: 'var(--color-warning-bg)',
      }}
    >
      <dl
        style={{
          display: 'grid',
          gridTemplateColumns: 'auto 1fr',
          gap: '0.25rem 0.6rem',
          margin: 0,
          fontSize: '0.76rem',
          color: 'var(--color-warning-fg)',
        }}
      >
        <dt style={{ fontWeight: 600 }}>Atom</dt>
        <dd style={{ margin: 0, fontFamily: 'ui-monospace, monospace' }}>
          {blockerKind.atom_id}
        </dd>
        <dt style={{ fontWeight: 600 }}>Atom wants</dt>
        <dd style={{ margin: 0 }}>
          {networkPolicyLabel(blockerKind.atom_network)}
        </dd>
        <dt style={{ fontWeight: 600 }}>Executor enforces</dt>
        <dd style={{ margin: 0 }}>
          {networkPolicyLabel(blockerKind.executor_network)}
        </dd>
      </dl>
      <div
        style={{
          marginTop: '0.5rem',
          display: 'flex',
          gap: '0.4rem',
          flexWrap: 'wrap',
        }}
      >
        <button
          type="button"
          onClick={() => {
            window.dispatchEvent(new CustomEvent('ecaax:open-settings'))
          }}
          data-testid="switch-executor-button-network"
          style={{
            padding: '0.35rem 0.75rem',
            background: 'var(--color-warning-accent)',
            color: 'var(--color-text-on-accent)',
            border: 'none',
            borderRadius: 4,
            fontSize: '0.76rem',
            fontWeight: 600,
            cursor: 'pointer',
          }}
        >
          Switch executor
        </button>
        <button
          type="button"
          disabled
          title="Amend-atom-safety flow coming in a future release"
          data-testid="amend-atom-safety-button"
          style={{
            padding: '0.35rem 0.75rem',
            background: 'transparent',
            color: 'var(--color-warning-fg)',
            border: '1px solid var(--color-warning-border)',
            borderRadius: 4,
            fontSize: '0.76rem',
            fontWeight: 600,
            cursor: 'not-allowed',
            opacity: 0.6,
          }}
        >
          Amend atom safety
        </button>
      </div>
    </section>
  )
}

/// Atom-safety-policy dedicated panel for the
/// `provisioning_denied` dispatch-time gate. Shows the package +
/// registry the atom asked for and offers a "Add `<package>` to
/// atom.runtime_packages" affordance. The button POSTs to the Task 4.7
/// endpoint; until that lands a non-2xx surfaces in the local error
/// state (a brief inline message).
function ProvisioningDeniedSection({
  blockerKind,
  sessionId,
}: {
  blockerKind: Extract<BlockerKind, { kind: 'provisioning_denied' }>
  sessionId: string | null
}): JSX.Element {
  const [busy, setBusy] = useState(false)
  const [message, setMessage] = useState<string | null>(null)
  const onAdd = async () => {
    if (!sessionId) {
      setMessage('Session id missing — cannot add the package.')
      return
    }
    setBusy(true)
    setMessage(null)
    try {
      await postAddRuntimePackage(
        sessionId,
        blockerKind.atom_id,
        blockerKind.package,
        blockerKind.registry,
      )
      setMessage(
        `Added ${blockerKind.package} to ${blockerKind.atom_id}.runtime_packages — retry dispatch when ready.`,
      )
    } catch (e) {
      // The add-runtime-package endpoint may not be wired yet;
      // voidFetch surfaces a 404 as `Error("404 Not Found from
      // /api/...")`. Detect that specific shape and show SME-safe
      // "rolling out" copy instead of leaking the raw HTTP error.
      // Everything else gets a sanitized fallback message (still
      // terse, still SME-readable).
      const raw = (e as Error).message || ''
      const is404 = /\b404\b/.test(raw) || /not\s*found/i.test(raw)
      if (is404) {
        setMessage(
          'Add-package flow is rolling out; please retry in a few minutes.',
        )
      } else {
        // Strip URLs / status-codey bits so SMEs see a clean message.
        const sanitized = raw
          .replace(/from\s+https?:\/\/\S+/gi, '')
          .replace(/from\s+\/\S+/g, '')
          .replace(/\b\d{3}\s+[A-Z][A-Za-z ]+\b/g, '')
          .trim()
        setMessage(
          `Couldn't add ${blockerKind.package}${sanitized ? `: ${sanitized}` : '.'}`,
        )
      }
    } finally {
      setBusy(false)
    }
  }
  return (
    <section
      aria-label="Provisioning denied"
      data-testid="provisioning-denied-detail"
      style={{
        marginTop: '0.6rem',
        padding: '0.5rem 0.75rem',
        border: '1px solid #fcd34d',
        borderRadius: 6,
        background: 'var(--color-warning-bg)',
      }}
    >
      <dl
        style={{
          display: 'grid',
          gridTemplateColumns: 'auto 1fr',
          gap: '0.25rem 0.6rem',
          margin: 0,
          fontSize: '0.76rem',
          color: 'var(--color-warning-fg)',
        }}
      >
        <dt style={{ fontWeight: 600 }}>Atom</dt>
        <dd style={{ margin: 0, fontFamily: 'ui-monospace, monospace' }}>
          {blockerKind.atom_id}
        </dd>
        <dt style={{ fontWeight: 600 }}>Package</dt>
        <dd style={{ margin: 0, fontFamily: 'ui-monospace, monospace' }}>
          {blockerKind.package}
        </dd>
        <dt style={{ fontWeight: 600 }}>Registry</dt>
        <dd style={{ margin: 0, fontFamily: 'ui-monospace, monospace' }}>
          {blockerKind.registry}
        </dd>
      </dl>
      <div
        style={{
          marginTop: '0.5rem',
          display: 'flex',
          gap: '0.4rem',
          flexWrap: 'wrap',
        }}
      >
        <button
          type="button"
          onClick={onAdd}
          disabled={busy}
          data-testid="add-runtime-package"
          style={{
            padding: '0.35rem 0.75rem',
            background: 'var(--color-warning-accent)',
            color: 'var(--color-text-on-accent)',
            border: 'none',
            borderRadius: 4,
            fontSize: '0.76rem',
            fontWeight: 600,
            cursor: busy ? 'not-allowed' : 'pointer',
            opacity: busy ? 0.6 : 1,
          }}
        >
          {busy
            ? 'Adding…'
            : `Add \`${blockerKind.package}\` to atom.runtime_packages`}
        </button>
      </div>
      {message && (
        <p
          role="status"
          style={{
            marginTop: '0.4rem',
            marginBottom: 0,
            fontSize: '0.74rem',
            color: 'var(--color-warning-fg)',
            fontStyle: 'italic',
          }}
        >
          {message}
        </p>
      )}
    </section>
  )
}

/// "Tried so far" — flat list of {method, result} attempts the agent
/// has already made. Heuristic colour: green chip if `result` text
/// contains "succeeded" / "ok" / "complete"; red otherwise. Helps the
/// SME spot whether a re-run is repeating a known-failing path.
function AttemptsHistory({ attempts }: { attempts: BlockerAttempt[] }): JSX.Element {
  const succeeded = (r: string) =>
    /succeed|success|ok\b|complete|finished/i.test(r)
  return (
    <details
      data-testid="blocker-attempts-history"
      style={{
        marginTop: '0.6rem',
        padding: '0.4rem 0.6rem',
        background: 'var(--color-surface-1)',
        border: '1px solid var(--color-warning-border)',
        borderRadius: 4,
      }}
    >
      <summary
        style={{
          fontSize: '0.74rem',
          color: 'var(--color-warning-fg)',
          fontWeight: 600,
          cursor: 'pointer',
        }}
      >
        Tried so far ({attempts.length})
      </summary>
      <ul
        style={{
          listStyle: 'none',
          padding: 0,
          margin: '0.4rem 0 0',
          display: 'flex',
          flexDirection: 'column',
          gap: '0.3rem',
        }}
      >
        {attempts.map((a, i) => {
          const ok = succeeded(a.result)
          return (
            <li
              key={i}
              style={{
                fontSize: '0.74rem',
                color: 'var(--color-text-primary)',
                display: 'flex',
                gap: '0.4rem',
                alignItems: 'flex-start',
              }}
            >
              <span
                aria-hidden
                style={{
                  flex: '0 0 auto',
                  padding: '1px 6px',
                  fontSize: '0.65rem',
                  fontWeight: 600,
                  borderRadius: 3,
                  background: ok ? 'var(--color-success-bg)' : 'var(--color-danger-bg)',
                  color: ok ? 'var(--color-success-fg)' : 'var(--color-danger-fg)',
                }}
              >
                {ok ? 'OK' : 'FAIL'}
              </span>
              <span style={{ flex: '1 1 auto' }}>
                <code style={{ fontFamily: 'ui-monospace, monospace' }}>{a.method}</code>
                {' — '}
                {a.result}
              </span>
            </li>
          )
        })}
      </ul>
    </details>
  )
}

/// Inline "Retry: <label>" affordance rendered when the agent's
/// blocker.json declares a `recoverable_action: { kind: rerun_script,
/// rel_path, label }`. Lives below the scrollable body so it's always
/// visible alongside the submit button.
function RerunScriptStrip({
  action,
  busy,
  message,
  onClick,
}: {
  action: NonNullable<AgentBlockerJson['recoverable_action']>
  busy: string | null
  message: string | null
  onClick: () => void
}): JSX.Element {
  const isBusy = busy === action.rel_path
  return (
    <div
      data-testid="blocker-rerun-script"
      style={{
        marginTop: '0.5rem',
        padding: '0.5rem 0.7rem',
        background: 'var(--color-info-bg, #eff6ff)',
        border: '1px solid var(--color-info-border, #bfdbfe)',
        borderRadius: 4,
        display: 'flex',
        flexDirection: 'column',
        gap: '0.3rem',
      }}
    >
      <div
        style={{
          display: 'flex',
          gap: '0.5rem',
          alignItems: 'center',
        }}
      >
        <button
          type="button"
          onClick={onClick}
          disabled={isBusy}
          style={{
            padding: '0.4rem 0.8rem',
            background: 'var(--color-info-accent, #3b82f6)',
            color: 'white',
            border: 'none',
            borderRadius: 4,
            cursor: isBusy ? 'not-allowed' : 'pointer',
            fontSize: '0.78rem',
            fontWeight: 600,
            opacity: isBusy ? 0.6 : 1,
          }}
        >
          {isBusy ? 'Re-launching…' : `Retry: ${action.label ?? action.rel_path}`}
        </button>
        <code
          style={{
            fontFamily: 'ui-monospace, monospace',
            fontSize: '0.72rem',
            color: 'var(--color-text-muted)',
          }}
        >
          scripts/{action.rel_path}
        </code>
      </div>
      {action.description && (
        <p
          style={{
            margin: 0,
            fontSize: '0.74rem',
            color: 'var(--color-text-secondary)',
          }}
        >
          {action.description}
        </p>
      )}
      {message && (
        <p
          role="status"
          style={{
            margin: 0,
            fontSize: '0.74rem',
            color: 'var(--color-text-secondary)',
            fontStyle: 'italic',
          }}
        >
          {message}
        </p>
      )}
    </div>
  )
}
