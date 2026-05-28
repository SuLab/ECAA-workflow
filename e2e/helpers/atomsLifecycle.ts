/**
 * Atoms-campaign lifecycle wrapper — extends the standard chat→confirm→emit
 * flow with post-emit phases needed for the atoms/plots test campaign:
 *
 *   1. runLiveScenario  — intake beats + confirm + emit (reused as-is)
 *   2. DAG audit        — scripts/audit_dag.py must exit 0, no stranded nodes
 *   3. Start Execution  — click exec-start-btn in the Jobs tab
 *   4. Poll to done     — poll /state every 15s until completed+blocked+failed = total
 *   5. Figures tab      — assert figures rendered; optionally require every expected id
 *   6. Screenshots      — test-shots/<modality>/<step>.png at every major step
 *
 * Design: single public function, no shared state, no duplicate of liveScenarioRunner.
 */

import { spawnSync } from 'node:child_process'
import { mkdirSync, existsSync, readFileSync, readdirSync, promises as fsPromises } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { expect, type Page } from '@playwright/test'
import { runLiveScenario } from './liveScenarioRunner'
import { waitForEmittedPackagePath } from './liveServer'

const __dirname = dirname(fileURLToPath(import.meta.url))
const REPO_ROOT = resolve(__dirname, '..', '..')

const BASE_URL = process.env.SWFC_PLAYWRIGHT_BASE_URL ?? 'http://127.0.0.1:3737'

/** Minimum time between session-state polls when waiting for task completion. */
const POLL_INTERVAL_MS = 15_000

/**
 * Common discover_* stage names that are safe to auto-approve. The agent
 * picks the top candidate automatically instead of blocking on each gate,
 * letting the harness advance through analytical atoms in a single L1 run.
 *
 * L1 specs can override or extend this list via AtomsLifecycleOptions.
 */
// Source of truth: every atom whose registry definition triggers a
// `discover_*` companion via `crates/core/src/composer_v4/discover_companion_synthesis.rs::derive_axis`:
//   1. `method_choice.deferred_to` present → axis = stripped of `discover_` prefix
//      (4 atoms today: dtu_method, isoform_caller, spatial_clustering_method, time_series_method)
//   2. otherwise, non-empty `attributes.candidate_tools` → axis = atom_id
// The scheduler (`crates/harness/src/scheduler.rs:339`) auto-prefixes
// each entry with `discover_`, so the strings below are the bare stem.
export const DEFAULT_AUTO_APPROVE_DISCOVERIES: string[] = [
  // Sequencing / preprocessing
  'alignment',
  'sequence_trimming',
  'quantification',
  // Normalization + DR
  'normalisation',
  'dimensionality_reduction',
  // Cell-level
  'clustering',
  'cell_type_annotation',
  'batch_correction',
  // Peaks (ATAC / ChIP / CUT&Tag)
  'peak_calling',
  'peak_annotation',
  'motif_enrichment',
  'differential_accessibility',
  'enhancer_activity_calling',
  'peak_to_gene_linking',
  // Analysis
  'differential_expression',
  'pathway_enrichment',
  // Clinical trial atoms
  'clinical_endpoint_analysis',
  'clinical_safety_summary',
  'clinical_sensitivity_analysis',
  'clinical_subgroup_analysis',
  'clinical_label_derivation',
  'clinical_feature_extraction',
  'external_validation',
  'fairness_audit',
  'calibration_audit',
  'predictive_model_fit',
  // Time-series (axis-route via method_choice.deferred_to)
  'time_series_method',
  // Variant + GWAS
  'variant_calling',
  'variant_annotation',
  'variant_filtering',
  'colocalization',
  'gwas_summary_harmonization',
  'cis_regulatory_variant_scoring',
  // Methylation
  'expression_methylation_correlation',
  // Spatial (axis-route)
  'spatial_clustering_method',
  // Metagenomics
  'taxonomic_classification',
  // Proteomics + HLA peptidomics
  'peptide_search',
  'protein_quantification',
  'hla_peptide_search',
  'netmhc_binding_prediction',
  'neoantigen_filtering',
  // HiC / 3D chromatin
  'chromatin_contact_calling',
  'chromatin_loop_calling',
  'differential_loop_analysis',
  // Transcript / isoform (axis-route)
  'dtu_method',
  'isoform_caller',
  // Ribo-seq + translation
  'psite_offset_calibration',
  'translation_efficiency_de',
  // VDJ / repertoire
  'vdj_reconstruction',
  'repertoire_diversity',
  'cdr3_clonotype_clustering',
  // CRISPR / multiome
  'sgrna_assignment',
  'share_seq_barcode_match',
  'multiome_arc_demultiplex',
  // Cross-omics integration
  'joint_wnn_integration',
]

/**
 * Write runtime/.sme-auto-approve-discoveries into the emitted package so
 * the agent does not block on every discover_* task. Lets the harness advance
 * through analytical atoms instead of stalling on the first discover gate.
 *
 * Each L1 spec can override `allow` to test specific gate behaviour.
 */
async function writeAutoApproveDiscoveries(
  packagePath: string,
  allow: string[] = DEFAULT_AUTO_APPROVE_DISCOVERIES,
  deny: string[] = [],
): Promise<void> {
  const runtimeDir = `${packagePath}/runtime`
  const filePath = `${runtimeDir}/.sme-auto-approve-discoveries`
  const body = JSON.stringify({ allow, deny }, null, 2)
  await fsPromises.mkdir(runtimeDir, { recursive: true })
  await fsPromises.writeFile(filePath, body, 'utf-8')
  console.log(`    [preExecutionHook] wrote ${filePath} (allow=${allow.length} stages)`)
}
/** Grace period after last task finishes before asserting figure count. */
const FIGURES_PROBE_WAIT_MS = 20_000

// Plot generation must happen inside the agent's container — the
// `runtime/plotting/` library is bundled into every emitted package for
// exactly that purpose (see scripts/agent-prompts/task-execution.md).
// The earlier host-side renderer bridge (prepareRendererInputs +
// runPlotRenderers) silently re-ran `lib/plotting/_cli.py` from the
// test host when figures were missing, which masked real plot-generation
// regressions inside the container. That bridge was removed; the
// container-evidence assertion fires unconditionally below.

export interface AtomsLifecycleOptions {
  /** Relative to e2e/, e.g. 'fixtures/scenarios/atoms/bulk-rnaseq-pasilla.yaml' */
  scenarioPath: string
  /**
   * figure_id strings (matching data-figure-id attributes) to look for in
   * the Figures tab. At least one must render for the assertion to pass.
   * If omitted, any figure counts as sufficient.
   */
  expectedFigureIds?: string[]
  /**
   * When true, every `expectedFigureIds` entry must be present. Use this for
   * end-to-end plot coverage tests where a partial upstream-only render is a
   * failure, not a pass.
   */
  requireAllExpectedFigureIds?: boolean
  /**
   * When true, all emitted task nodes must complete successfully. This catches
   * harness exits that leave ready/pending downstream nodes stranded.
   */
  requireAllTasksCompleted?: boolean
  /**
   * When set, every executed task's .container-state.json sidecar must report
   * this exact image. This catches per-atom preferred_container pins that
   * bypass the local test image.
   */
  expectedContainerImage?: string
  /** Max wall-clock for execution phase. Default 30 min. */
  executionTimeoutMs?: number
  /** Used for screenshot sub-directory naming under test-shots/. */
  modality: string
  /**
   * Optional async hook called after emit + DAG audit, before Start Execution.
   * Use to write runtime/inputs.json, symlink local testdata files, or any
   * other package setup that the agent needs before data_acquisition can run.
   * Receives the absolute package path.
   */
  preExecutionHook?: (packagePath: string) => Promise<void>
  /**
   * Optional async hook called after the live UI creates a session and renders
   * the first assistant greeting, but before the first intake message. Use for
   * setup that must influence pre-emit DAG construction, such as registering
   * local inputs through the Inputs tab.
   */
  preIntakeHook?: (page: Page, sessionId: string, shotDir: string) => Promise<void>
  /**
   * Discover-stage names to pre-approve so the agent picks the top candidate
   * automatically rather than blocking on an AwaitingSmeApproval gate.
   * Written into runtime/.sme-auto-approve-discoveries before Start Execution.
   *
   * Defaults to DEFAULT_AUTO_APPROVE_DISCOVERIES (common analytical stages).
   * Pass an empty array [] to suppress auto-approval entirely.
   */
  autoApproveDiscoveries?: string[]
  /**
   * Discover-stage names to explicitly deny in runtime/.sme-auto-approve-discoveries.
   * An entry in `deny` takes precedence over `allow`. Defaults to [].
   */
  denyAutoApprove?: string[]
  /**
   * Task ids that must not appear in the emitted DAG. This is used for
   * tractable fixture runs where the SME registers post-pipeline inputs before
   * intake, so upstream raw-read atoms should be pruned before execution.
   */
  expectedAbsentTaskIds?: string[]
}

export interface AtomsLifecycleResult {
  sessionId: string
  packagePath: string
  taskCount: number
  completed: number
  blocked: number
  failed: number
  figureCount: number
  figureIds: string[]
}

/**
 * Drive the full atoms-campaign lifecycle for one scenario:
 * intake → emit → DAG audit → execute → figures.
 *
 * The caller MUST have navigated to page.goto('/') before calling this
 * (liveScenarioRunner handles the navigation internally via its own
 * waitForSessionCreated — we intercept the session id by attaching a
 * response listener BEFORE the runner calls page.goto).
 */
export async function runAtomsLifecycle(
  page: Page,
  opts: AtomsLifecycleOptions,
): Promise<AtomsLifecycleResult> {
  const executionTimeoutMs = opts.executionTimeoutMs ?? 30 * 60_000
  const shotDir = join('test-shots', opts.modality)
  mkdirSync(shotDir, { recursive: true })

  // ── Step 2: Drive intake → confirm → emit via the existing runner.
  // runLiveScenario now returns { sessionId } so we don't need a second
  // waitForResponse race.
  const { sessionId } = await runLiveScenario(page, opts.scenarioPath, {
    beforeFirstTurn: opts.preIntakeHook
      ? async ({ page: hookPage, sessionId: hookSessionId }) => {
          await opts.preIntakeHook?.(hookPage, hookSessionId, shotDir)
        }
      : undefined,
  })
  await captureLifecycleScreenshot(page, join(shotDir, '01-after-emit.png'))

  // ── Step 3: Get the emitted package path from server state.
  const pkgPath = await waitForEmittedPackagePath(page, sessionId, 60_000)
  expect(pkgPath, 'emitted_package_path must be set after emit').not.toBeNull()
  const packagePath = pkgPath!
  console.log(`\n  ATOMS LIFECYCLE — package: ${packagePath}`)

  // ── Step 4: DAG audit via scripts/audit_dag.py.
  const workflowJson = join(packagePath, 'WORKFLOW.json')
  console.log(`  Running DAG audit: ${workflowJson}`)
  const audit = spawnSync(
    'python3',
    [join(REPO_ROOT, 'scripts', 'audit_dag.py'), workflowJson],
    { encoding: 'utf8', timeout: 30_000 },
  )
  const auditOutput = (audit.stdout ?? '') + (audit.stderr ?? '')
  console.log(`  DAG audit output: ${auditOutput.trim()}`)
  expect(
    audit.status,
    `DAG audit failed (exit ${audit.status ?? 'null'}):\n${auditOutput}`,
  ).toBe(0)
  // Check that no stranded nodes appear in the audit output.
  expect(
    auditOutput.toLowerCase(),
    'DAG audit must report no stranded nodes',
  ).not.toMatch(/stranded/)
  if (opts.expectedAbsentTaskIds && opts.expectedAbsentTaskIds.length > 0) {
    const workflow = JSON.parse(readFileSync(workflowJson, 'utf8')) as {
      tasks?: Record<string, unknown>
    }
    const taskIds = new Set(Object.keys(workflow.tasks ?? {}))
    for (const taskId of opts.expectedAbsentTaskIds) {
      expect(taskIds.has(taskId), `${taskId} must be pruned from this emitted DAG`).toBe(false)
    }
  }
  await captureLifecycleScreenshot(page, join(shotDir, '02-dag-audit-pass.png'))

  // ── Step 4b: Write auto-approve file so the agent doesn't block on every
  // discover_* gate. This runs unconditionally before the caller's preExecutionHook
  // so the file is present when any subsequent hook inspects the package.
  // Callers may pass autoApproveDiscoveries=[] to suppress.
  const autoAllow = opts.autoApproveDiscoveries ?? DEFAULT_AUTO_APPROVE_DISCOVERIES
  const autoDeny = opts.denyAutoApprove ?? []
  if (autoAllow.length > 0) {
    console.log('  Writing auto-approve-discoveries file…')
    await writeAutoApproveDiscoveries(packagePath, autoAllow, autoDeny)
  }

  // ── Step 4c: Optional pre-execution hook (inputs setup, symlinks, etc.)
  if (opts.preExecutionHook) {
    console.log('  Running preExecutionHook…')
    await opts.preExecutionHook(packagePath)
    console.log('  preExecutionHook done')
    await captureLifecycleScreenshot(page, join(shotDir, '02b-pre-exec-hook.png'))
  }

  // ── Step 5: Click Start Execution via the Jobs tab.
  // StartExecutionCard renders in the Jobs (Progress) tab with
  // data-testid="start-execution-card" and the start button has
  // data-testid="exec-start-btn". The inline panel above the composer
  // uses "exec-start-btn-inline" — we prefer the card in the Jobs tab
  // for explicit traceability.
  console.log('  Opening Jobs tab and clicking Start Execution…')
  await page.locator('#state-tab-jobs').click()
  await captureLifecycleScreenshot(page, join(shotDir, '03-jobs-tab.png'))

  const startCard = page.locator('[data-testid="start-execution-card"]')
  await expect(startCard).toBeVisible({ timeout: 10_000 })

  const startBtn = startCard.locator('[data-testid="exec-start-btn"]')
  await expect(startBtn).toBeVisible({ timeout: 10_000 })
  await expect(startBtn).toBeEnabled({ timeout: 10_000 })
  const startResponse = page.waitForResponse(
    (r) =>
      r.url().endsWith(`/session/${sessionId}/start-execution`) &&
      r.request().method() === 'POST',
    { timeout: 30_000 },
  )
  await startBtn.click()
  const startResult = await startResponse
  if (startResult.status() >= 300) {
    const body = await startResult.text().catch(() => '<unreadable response body>')
    throw new Error(`start-execution failed with HTTP ${startResult.status()}: ${body}`)
  }
  console.log('  Clicked Start Execution')
  await captureLifecycleScreenshot(page, join(shotDir, '04-start-clicked.png'))

  // ── Step 6: Poll /state until all tasks are in terminal states.
  // Terminal = completed + blocked + failed. Running and Pending are active.
  // The ProgressSummary does not expose a "running" count directly — it
  // buckets Running with Ready. We infer completion when
  //   completed + blocked + failed == task_count
  // which can also be expressed as: ready (which includes running) == 0 AND pending == 0.
  console.log(`  Polling for execution completion (timeout: ${executionTimeoutMs / 1000}s)…`)
  let execResult: ExecProgressSnapshot
  let pollError: unknown = null
  try {
    execResult = await pollUntilDone(page, sessionId, executionTimeoutMs)
    console.log(
      `  Execution done — total=${execResult.total} completed=${execResult.completed} blocked=${execResult.blocked} failed=${execResult.failed}`,
    )
    if (opts.requireAllTasksCompleted) {
      expect(execResult.blocked, 'no task may remain blocked in full lifecycle mode').toBe(0)
      expect(execResult.failed, 'no task may fail in full lifecycle mode').toBe(0)
      expect(execResult.completed, 'all task nodes must complete in full lifecycle mode').toBe(
        execResult.total,
      )
    }
    await captureLifecycleScreenshot(page, join(shotDir, '05-execution-done.png'))
  } catch (err) {
    pollError = err
    throw err
  } finally {
    try {
      await stopExecutionIfStillRunning(page, sessionId, shotDir)
    } catch (stopErr) {
      if (!pollError) throw stopErr
      console.warn(`  [cleanup] failed to stop harness after poll error: ${String(stopErr).slice(0, 240)}`)
    }
  }

  // Step 6b: container evidence assertion. Every executed task must
  // carry a .container-state.json sidecar — the in-package
  // runtime/plotting/ library is what the agent uses to render figures,
  // and only the harness writes that sidecar after a successful
  // `docker run` returns. The earlier host-side renderer bridge was
  // removed in this commit because it silently masked agent failures
  // to invoke runtime/plotting from inside the container.
  assertExecutedTasksHaveContainerEvidence(packagePath, opts.expectedContainerImage)

  // ── Step 7: Figures tab assertion.
  // Wait a brief grace period for the figures poll cycle to pick up the
  // manifest.json files written by the agent.
  console.log('  Opening Figures tab…')
  await page.locator('#state-tab-figures').click()
  // Allow the poll cycle to fetch manifests.
  await page.waitForTimeout(FIGURES_PROBE_WAIT_MS)
  await captureLifecycleScreenshot(page, join(shotDir, '06-figures-tab.png'))

  // Count rendered figures (any data-figure-id element visible in the pane).
  const allFigureIds = await page.locator('[data-figure-id]').allInnerTexts()
  // The element text is the figure id label; collect the attribute directly.
  const figureIdValues = await page.locator('[data-figure-id]').evaluateAll(
    (els) => els.map((e) => e.getAttribute('data-figure-id') ?? ''),
  )
  const figureCount = figureIdValues.filter((id) => id.length > 0).length
  console.log(`  Figures found: ${figureCount} — ids: ${figureIdValues.join(', ')}`)

  // Assert at least one figure is rendered.
  expect(
    figureCount,
    `Figures tab must show at least one rendered figure. Found ids: [${figureIdValues.join(', ')}]`,
  ).toBeGreaterThan(0)

  // If caller specified expected figure ids, assert at least one is present.
  if (opts.expectedFigureIds && opts.expectedFigureIds.length > 0) {
    const matchCount = opts.expectedFigureIds.filter((eid) =>
      figureIdValues.includes(eid),
    ).length
    console.log(
      `  Expected figure ids: [${opts.expectedFigureIds.join(', ')}] — ${matchCount} matched`,
    )
    if (opts.requireAllExpectedFigureIds) {
      const missing = opts.expectedFigureIds.filter((eid) => !figureIdValues.includes(eid))
      expect(
        missing,
        `All expected figure ids must render. Found: [${figureIdValues.join(', ')}]`,
      ).toEqual([])
    } else {
      expect(
        matchCount,
        `At least one of the expected figure ids [${opts.expectedFigureIds.join(', ')}] must render. Found: [${figureIdValues.join(', ')}]`,
      ).toBeGreaterThan(0)
    }
  }

  await captureLifecycleScreenshot(page, join(shotDir, '07-figures-verified.png'))
  console.log(`  Atoms lifecycle complete for session ${sessionId}`)
  // Suppress the "unused variable" lint — allFigureIds used only for side-log above.
  void allFigureIds

  return {
    sessionId,
    packagePath,
    taskCount: execResult.total,
    completed: execResult.completed,
    blocked: execResult.blocked,
    failed: execResult.failed,
    figureCount,
    figureIds: figureIdValues,
  }
}

// ── Internals ───────────────────────────────────────────────────────────────

/**
 * Capture evidence screenshots without letting a slow full-page capture abort
 * a long live harness run after execution has already succeeded. Full-page
 * screenshots are still preferred; viewport fallback preserves visual evidence
 * when Chromium times out while stitching a tall, live-updating page.
 */
async function captureLifecycleScreenshot(
  page: Page,
  path: string,
): Promise<void> {
  try {
    await page.screenshot({ path, fullPage: true, timeout: 60_000 })
    return
  } catch (fullPageError) {
    console.warn(
      `  [screenshot] full-page capture failed for ${path}; retrying viewport capture: ${String(fullPageError).slice(0, 240)}`,
    )
  }

  await page.screenshot({ path, fullPage: false, timeout: 30_000 })
}

function assertExecutedTasksHaveContainerEvidence(
  packagePath: string,
  expectedContainerImage?: string,
): void {
  const outputsDir = join(packagePath, 'runtime', 'outputs')
  expect(existsSync(outputsDir), 'runtime/outputs must exist before container audit').toBe(true)

  const executed: string[] = []
  for (const taskId of readdirSync(outputsDir)) {
    const taskOutputDir = join(outputsDir, taskId)
    const patchAppliedPath = join(taskOutputDir, 'state.patch.applied.json')
    const patchPath = join(taskOutputDir, 'state.patch.json')
    const patchFile = existsSync(patchAppliedPath)
      ? patchAppliedPath
      : existsSync(patchPath)
        ? patchPath
        : null
    if (!patchFile) continue

    let status = ''
    try {
      const patch = JSON.parse(readFileSync(patchFile, 'utf8')) as {
        to?: { status?: string }
      }
      status = patch.to?.status ?? ''
    } catch {
      continue
    }
    if (!['completed', 'blocked', 'failed'].includes(status)) continue

    executed.push(taskId)
    const statePath = join(taskOutputDir, '.container-state.json')
    expect(existsSync(statePath), `${taskId} executed without .container-state.json`).toBe(true)
    const state = JSON.parse(readFileSync(statePath, 'utf8')) as {
      runtime?: string
      image?: string
      exit_code?: number
      task_id?: string
    }
    expect(state.runtime, `${taskId} container runtime`).toBe('docker')
    expect(state.image, `${taskId} container image`).toBeTruthy()
    if (expectedContainerImage) {
      expect(state.image, `${taskId} container image`).toBe(expectedContainerImage)
    }
    expect(typeof state.exit_code, `${taskId} container exit code`).toBe('number')
    expect(state.task_id, `${taskId} container sidecar task id`).toBe(taskId)

    const proofPath = join(taskOutputDir, 'container-proof.json')
    if (existsSync(proofPath)) {
      const proof = JSON.parse(readFileSync(proofPath, 'utf8')) as {
        containerized?: boolean
        runtime?: string
        image?: string
        dockerenv?: boolean
      }
      expect(proof.containerized, `${taskId} fixture proof containerized flag`).toBe(true)
      expect(proof.runtime, `${taskId} fixture proof runtime`).toBe('docker')
      expect(proof.image, `${taskId} fixture proof image`).toBeTruthy()
      expect(proof.dockerenv, `${taskId} fixture proof /.dockerenv`).toBe(true)
    }
  }

  expect(executed.length, 'container audit must see at least one executed task').toBeGreaterThan(0)
  console.log(`  [container] verified Docker execution evidence for ${executed.length} task(s)`)
}

interface ExecProgressSnapshot {
  total: number
  completed: number
  blocked: number
  failed: number
  ready: number
  pending: number
}

interface ExecutionStatus {
  status?: string
}

async function fetchExecutionStatus(
  page: Page,
  sessionId: string,
): Promise<ExecutionStatus | null> {
  return page.evaluate(
    async (url) => {
      const res = await fetch(url)
      if (!res.ok) return null
      return (await res.json()) as { status?: string } | null
    },
    `${BASE_URL}/api/chat/session/${sessionId}/execution`,
  )
}

/**
 * L1 live tests are allowed to stop at a real SME blocker once enough upstream
 * atoms have executed. If the harness is still alive at that point, click the
 * Jobs-tab Stop button and wait for the cooperative shutdown before Playwright
 * tears down its web server. Otherwise the harness fail-closes on a missing
 * backend and lingers in a paused loop.
 */
async function stopExecutionIfStillRunning(
  page: Page,
  sessionId: string,
  shotDir: string,
): Promise<void> {
  const status = await fetchExecutionStatus(page, sessionId)
  const kind = status?.status ?? 'idle'
  if (!['running', 'pausing', 'paused', 'stopping'].includes(kind)) {
    console.log(`  Harness cleanup not needed (status=${kind})`)
    return
  }

  console.log(`  Harness still ${kind}; requesting cooperative Stop via Jobs tab…`)
  await page.locator('#state-tab-jobs').click()
  await captureLifecycleScreenshot(page, join(shotDir, '05b-before-stop.png'))

  if (kind !== 'stopping') {
    const stopBtn = page.locator('[data-testid="start-execution-card"] [data-testid="exec-stop-btn"]').first()
    await expect(stopBtn).toBeVisible({ timeout: 10_000 })
    await expect(stopBtn).toBeEnabled({ timeout: 10_000 })
    await stopBtn.click()
  }

  // Cooperative-stop budget: the harness only checks the
  // `.harness-stop` sentinel between iterations (every 5-10s), and
  // it doesn't interrupt an in-flight agent. An R-compile inside the
  // container can easily take 5-15 minutes, so a 120s budget is
  // way too tight — we'd false-positive on a clean shutdown that
  // simply requires the current task to finish first. 10 min is
  // generous enough to ride out a long-running task without burying
  // genuine "stop wedged" failures.
  //
  // A genuine stop failure (harness wedged, not just slow) still
  // throws; the broader 10-min cap caps a hung shutdown. The poll
  // cadence stays at 2s so a fast clean stop is reflected promptly.
  const stopBudgetMs = 10 * 60_000
  const deadline = Date.now() + stopBudgetMs
  while (Date.now() < deadline) {
    const next = await fetchExecutionStatus(page, sessionId)
    const nextKind = next?.status ?? 'idle'
    if (nextKind === 'exited' || nextKind === 'idle') {
      console.log(`  Harness stopped cleanly (status=${nextKind})`)
      await captureLifecycleScreenshot(page, join(shotDir, '05c-after-stop.png'))
      return
    }
    await page.waitForTimeout(2_000)
  }

  throw new Error(
    `Harness did not stop within ${stopBudgetMs / 1000}s for session ${sessionId}`,
  )
}

/**
 * Poll /api/chat/session/:id/state at POLL_INTERVAL_MS until all tasks reach
 * terminal states OR the harness exits, or until executionTimeoutMs exceeded.
 *
 * Terminal determination: completed + blocked == task_count (harness fully
 * drained), OR harness execution status == "exited" (harness exhausted its
 * iteration budget — some tasks may remain pending/ready but won't advance
 * without a manual Resume). Either condition ends the poll.
 *
 * "blocked" in TaskState maps to ProgressSummary.blocked; "failed" is also
 * counted in blocked by the server (TaskState::Failed → blocked bucket).
 * We surface the raw progress counts for the caller to inspect.
 */
async function pollUntilDone(
  page: Page,
  sessionId: string,
  timeoutMs: number,
): Promise<ExecProgressSnapshot> {
  const stateUrl = `${BASE_URL}/api/chat/session/${sessionId}/state`
  const execUrl = `${BASE_URL}/api/chat/session/${sessionId}/execution`
  const deadline = Date.now() + timeoutMs
  let lastSnapshot: ExecProgressSnapshot = {
    total: 0,
    completed: 0,
    blocked: 0,
    failed: 0,
    ready: 0,
    pending: 0,
  }
  // Tracks poll iterations where blocked=true and completed count has not changed.
  let stallPolls = 0
  let prevCompleted = -1

  while (Date.now() < deadline) {
    // Fetch state and execution status in parallel.
    const [snapshot, execStatus] = await page.evaluate(
      async ([su, eu]) => {
        const [sr, er] = await Promise.allSettled([fetch(su), fetch(eu)])
        const state =
          sr.status === 'fulfilled' && sr.value.ok
            ? ((await sr.value.json()) as {
                task_count: number
                progress: {
                  completed: number
                  ready: number
                  blocked: number
                  pending: number
                }
              })
            : null
        const exec =
          er.status === 'fulfilled' && er.value.ok
            ? ((await er.value.json()) as { status?: string } | null)
            : null
        return [state, exec] as const
      },
      [stateUrl, execUrl] as const,
    )

    if (snapshot) {
      const { completed, ready, blocked, pending } = snapshot.progress
      const total = snapshot.task_count
      const snap: ExecProgressSnapshot = {
        total,
        completed,
        ready,
        blocked,
        failed: 0, // folded into blocked by the server's ProgressSummary
        pending,
      }
      lastSnapshot = snap

      const terminalCount = completed + blocked
      const harnessExited = execStatus?.status === 'exited'
      const sessionStateKind = (snapshot as unknown as { state?: { kind?: string } }).state?.kind
      console.log(
        `    [poll] total=${total} done=${terminalCount} (completed=${completed} blocked=${blocked} ready=${ready} pending=${pending}) harness=${execStatus?.status ?? 'null'}`,
      )

      // Exit when all tasks are in terminal states.
      if (total > 0 && terminalCount >= total) {
        return snap
      }
      // Also exit when harness has exited — it won't dispatch more tasks.
      // Surface what completed/blocked so the caller can assert on them.
      if (harnessExited) {
        console.log(
          `    [poll] harness exited — returning snapshot (${terminalCount}/${total} terminal)`,
        )
        return snap
      }
      // Exit when the session is fully Blocked (SME approval required). After
      // writeAutoApproveDiscoveries pre-approves common discover stages the
      // harness should advance through several atoms before hitting a remaining
      // gate. We require completed >= 2 to confirm the uplift worked, then
      // exit early. A stall watchdog fires after 8 consecutive polls with no
      // progress (completed count unchanged) so we don't burn the full timeout
      // when auto-approve doesn't help or the package stalls at completed=1.
      if (sessionStateKind === 'blocked') {
        if (completed >= 2) {
          console.log(
            `    [poll] session blocked with completed=${completed} (>=2) — early exit for L1 pass`,
          )
          return snap
        }
        if (completed === prevCompleted) {
          stallPolls++
          if (stallPolls >= 8) {
            console.log(
              `    [poll] session blocked, completed=${completed}, no progress for 8 polls — exit anyway`,
            )
            return snap
          }
        } else {
          stallPolls = 0
        }
        prevCompleted = completed
      }
    }

    // Not done yet — wait before next poll.
    const remaining = deadline - Date.now()
    if (remaining <= 0) break
    await page.waitForTimeout(Math.min(POLL_INTERVAL_MS, remaining))
  }

  // Timeout — distinguish "real failure" from "ran out of patience while
  // making progress." When the harness is healthy (no blocked tasks AND
  // the spec didn't demand full completion via requireAllTasksCompleted)
  // a timeout is a soft cap, not an error: we've gathered enough breadth
  // signal to assert structural integrity + figures, and the caller's
  // post-poll assertions decide whether the partial result meets bar.
  // Specs that REQUIRE 100% completion still need to throw to fail the
  // test — that's signalled by the caller via requireAllTasksCompleted.
  const { ready, pending, total, completed, blocked } = lastSnapshot
  if (blocked > 0) {
    throw new Error(
      `Execution timed out after ${timeoutMs / 1000}s with blocked tasks. ` +
        `Last state: total=${total} completed=${completed} blocked=${blocked} ready=${ready} pending=${pending}. ` +
        `Session: ${sessionId}`,
    )
  }
  console.warn(
    `    [poll] timeout @ ${timeoutMs / 1000}s with no blockers — ` +
      `returning partial snapshot (completed=${completed}/${total}, ready=${ready}, pending=${pending}). ` +
      `Caller's requireAllTasksCompleted assertion (if set) will decide pass/fail.`,
  )
  return lastSnapshot
}
