import { spawnSync } from 'node:child_process'
import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  writeFileSync,
} from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { expect, test, type Page } from '@playwright/test'
import {
  DEFAULT_AUTO_APPROVE_DISCOVERIES,
  runAtomsLifecycle,
  type AtomsLifecycleResult,
} from '../../helpers/atomsLifecycle'
import {
  registerPasillaInputViaUi,
  wirePasillaRuntimeInputs,
} from '../../helpers/pasillaFixture'
import { waitForEmittedPackagePath } from '../../helpers/liveServer'

const __dirname = dirname(fileURLToPath(import.meta.url))
const REPO_ROOT = resolve(__dirname, '..', '..', '..')
const BASE_URL = process.env.ECAA_PLAYWRIGHT_BASE_URL ?? 'http://127.0.0.1:3737'
const E2E_SESSIONS_DIR = '/tmp/scripps-e2e-sessions'
const EXPECTED_IMAGE = 'bio-min:local'
const PASILLA_SCENARIO = 'fixtures/scenarios/atoms/bulk-rnaseq-pasilla.yaml'

test.describe.configure({ mode: 'serial' })

test.describe('live atom lifecycle', () => {
  test('bulk RNA-seq pasilla executes locally, renders eligible plots, and branches at a task boundary', async ({
    page,
  }) => {
    const parent = await runPasillaLifecycle(page, 'live-bulk-rnaseq-pasilla')
    assertAllRequiredFiguresRendered(parent.packagePath, parent.figureIds)
    assertCompletionOrderRespectsDependencies(parent.packagePath, {
      requireEveryTaskEvent: true,
    })
    ensurePackageGitCommitted(parent.packagePath, 'test: commit live pasilla artifacts')

    await createExecuteAndVerifyTaskBranch(
      page,
      parent,
      'differential_expression',
    )
    ensurePackageGitCommitted(
      parent.packagePath,
      'test: commit live pasilla branch decision artifacts',
    )
  })
})

async function runPasillaLifecycle(
  page: Page,
  modality: string,
): Promise<AtomsLifecycleResult> {
  return runAtomsLifecycle(page, {
    scenarioPath: PASILLA_SCENARIO,
    modality,
    requireAllTasksCompleted: true,
    expectedContainerImage: EXPECTED_IMAGE,
    expectedFigureIds: ['volcano', 'summary_dashboard'],
    preIntakeHook: async (hookPage, _sessionId, shotDir) => {
      await registerPasillaInputViaUi(
        hookPage,
        join(shotDir, '00-pasilla-input-registered.png'),
      )
    },
    preExecutionHook: wirePasillaRuntimeInputs,
    executionTimeoutMs: 45 * 60_000,
  })
}

async function createExecuteAndVerifyTaskBranch(
  page: Page,
  parent: AtomsLifecycleResult,
  taskId: string,
): Promise<void> {
  const shotDir = join('test-shots', 'live-bulk-rnaseq-pasilla-branch')
  mkdirSync(shotDir, { recursive: true })

  await page.locator('#state-tab-plan').click()
  await captureScreenshot(page, join(shotDir, '08-parent-plan-before-branch.png'))

  const taskNode = page
    .getByRole('button', { name: new RegExp(`Task ${escapeRegExp(taskId)}\\b`) })
    .first()
  await expect(taskNode, `DAG node for ${taskId} must be visible`).toBeVisible({
    timeout: 20_000,
  })
  await taskNode.click()

  const drawer = page.getByTestId('task-detail-drawer')
  await expect(drawer).toBeVisible({ timeout: 10_000 })
  await captureScreenshot(page, join(shotDir, '09-task-drawer.png'))

  const branchResponsePromise = page.waitForResponse(
    (response) =>
      response.url().endsWith(`/session/${parent.sessionId}/branch`) &&
      response.request().method() === 'POST',
    { timeout: 30_000 },
  )
  const branchUrlPromise = page.waitForURL(
    (url) => {
      const child = url.searchParams.get('session')
      return Boolean(child && child !== parent.sessionId)
    },
    { timeout: 30_000 },
  )

  await drawer.getByRole('button', { name: /explore in a branch/i }).click()
  await captureScreenshot(page, join(shotDir, '10-branch-modal.png'))

  const rationale = page.locator('textarea').last()
  await expect(rationale).toBeVisible({ timeout: 10_000 })
  await rationale.fill(
    `Branch from ${taskId} to exercise downstream re-execution with the local fixture agent.`,
  )
  await page.getByRole('button', { name: /^create branch$/i }).click()

  const branchResponse = await branchResponsePromise
  if (branchResponse.status() >= 300) {
    const body = await branchResponse.text().catch(() => '<unreadable>')
    throw new Error(`branch failed with HTTP ${branchResponse.status()}: ${body}`)
  }
  await branchUrlPromise
  const childSessionId = new URL(page.url()).searchParams.get('session')
  expect(childSessionId, 'branch navigation must include child session id').toBeTruthy()

  if (!page.url().includes(`session=${encodeURIComponent(childSessionId!)}`)) {
    await page.goto(
      `${BASE_URL}/?session=${encodeURIComponent(childSessionId!)}&branched_from=${encodeURIComponent(parent.sessionId)}`,
    )
  }

  const childPackagePath = await waitForEmittedPackagePath(
    page,
    childSessionId!,
    120_000,
  )
  expect(childPackagePath, 'branch child must auto-emit a package').toBeTruthy()
  expect(childPackagePath).not.toBe(parent.packagePath)

  const childState = await fetchSessionState(page, childSessionId!)
  expect(childState.parent_session_id).toBe(parent.sessionId)
  assertSessionFileLineage(childSessionId!, parent.sessionId, taskId)

  await page.locator('#state-tab-plan').click()
  await expect(page.getByText(/branched session/i)).toBeVisible({
    timeout: 20_000,
  })
  await captureScreenshot(page, join(shotDir, '11-child-plan.png'))

  auditDag(childPackagePath!)
  assertNoIsolatedNodes(childPackagePath!)
  assertBranchBoundaryReset(childPackagePath!, taskId)
  writeAutoApproveDiscoveries(childPackagePath!)

  const childFigures = await startAndWaitForLocalExecution(
    page,
    childSessionId!,
    shotDir,
    35 * 60_000,
  )

  const childExpectedFigures = assertAllRequiredFiguresRendered(
    childPackagePath!,
    childFigures,
  )
  expect(childExpectedFigures).toEqual(
    expect.arrayContaining(['volcano', 'summary_dashboard']),
  )
  assertCompletionOrderRespectsDependencies(childPackagePath!, {
    requireEveryTaskEvent: false,
  })
  assertExecutedTasksHaveContainerEvidence(childPackagePath!, EXPECTED_IMAGE)
  ensurePackageGitCommitted(
    childPackagePath!,
    'test: commit branched pasilla artifacts',
  )
}

async function startAndWaitForLocalExecution(
  page: Page,
  sessionId: string,
  shotDir: string,
  timeoutMs: number,
): Promise<string[]> {
  await page.locator('#state-tab-jobs').click()
  await captureScreenshot(page, join(shotDir, '12-child-jobs-before-start.png'))

  const startCard = page.getByTestId('start-execution-card')
  await expect(startCard).toBeVisible({ timeout: 20_000 })
  const startButton = startCard.getByTestId('exec-start-btn')
  await expect(startButton).toBeVisible({ timeout: 20_000 })
  await expect(startButton).toBeEnabled({ timeout: 20_000 })

  const startResponsePromise = page.waitForResponse(
    (response) =>
      response.url().endsWith(`/session/${sessionId}/start-execution`) &&
      response.request().method() === 'POST',
    { timeout: 30_000 },
  )
  await startButton.click()
  const startResponse = await startResponsePromise
  if (startResponse.status() >= 300) {
    const body = await startResponse.text().catch(() => '<unreadable>')
    throw new Error(
      `branch start-execution failed with HTTP ${startResponse.status()}: ${body}`,
    )
  }
  await captureScreenshot(page, join(shotDir, '13-child-start-clicked.png'))

  const done = await pollUntilAllTasksComplete(page, sessionId, timeoutMs)
  expect(done.blocked, 'branch child must not leave blocked tasks').toBe(0)
  expect(done.completed, 'branch child must complete every task').toBe(done.total)

  await page.locator('#state-tab-figures').click()
  await expectNoDocumentHorizontalScroll(page)
  await page.waitForTimeout(20_000)
  await captureScreenshot(page, join(shotDir, '14-child-figures.png'))
  return page.locator('[data-figure-id]').evaluateAll((elements) =>
    elements
      .map((element) => element.getAttribute('data-figure-id') ?? '')
      .filter((id) => id.length > 0),
  )
}

async function pollUntilAllTasksComplete(
  page: Page,
  sessionId: string,
  timeoutMs: number,
): Promise<ProgressSnapshot> {
  const deadline = Date.now() + timeoutMs
  let last: ProgressSnapshot = {
    total: 0,
    completed: 0,
    blocked: 0,
    ready: 0,
    pending: 0,
  }
  while (Date.now() < deadline) {
    const snapshot = await fetchSessionState(page, sessionId)
    const progress = snapshot.progress ?? {}
    last = {
      total: Number(snapshot.task_count ?? 0),
      completed: Number(progress.completed ?? 0),
      blocked: Number(progress.blocked ?? 0),
      ready: Number(progress.ready ?? 0),
      pending: Number(progress.pending ?? 0),
    }
    console.log(
      `    [branch poll] total=${last.total} completed=${last.completed} blocked=${last.blocked} ready=${last.ready} pending=${last.pending}`,
    )
    if (last.total > 0 && last.completed + last.blocked >= last.total) {
      return last
    }
    await page.waitForTimeout(10_000)
  }
  throw new Error(
    `branch execution timed out for ${sessionId}: total=${last.total} completed=${last.completed} blocked=${last.blocked} ready=${last.ready} pending=${last.pending}`,
  )
}

interface ProgressSnapshot {
  total: number
  completed: number
  blocked: number
  ready: number
  pending: number
}

async function fetchSessionState(page: Page, sessionId: string): Promise<any> {
  return page.evaluate(async (url) => {
    const response = await fetch(url)
    if (!response.ok) {
      throw new Error(`state fetch failed: HTTP ${response.status}`)
    }
    return response.json()
  }, `${BASE_URL}/api/chat/session/${sessionId}/state`)
}

function auditDag(packagePath: string): void {
  const workflowJson = join(packagePath, 'WORKFLOW.json')
  const audit = spawnSync(
    'python3',
    [join(REPO_ROOT, 'scripts', 'audit_dag.py'), workflowJson],
    { encoding: 'utf8', timeout: 30_000 },
  )
  const output = `${audit.stdout ?? ''}${audit.stderr ?? ''}`
  console.log(output.trim())
  expect(audit.status, `DAG audit failed:\n${output}`).toBe(0)
  expect(output.toLowerCase()).not.toMatch(/stranded/)
}

function assertNoIsolatedNodes(packagePath: string): void {
  const workflow = readWorkflow(packagePath)
  const tasks = workflow.tasks ?? {}
  const outgoing: Record<string, number> = {}
  for (const taskId of Object.keys(tasks)) outgoing[taskId] = 0
  for (const [taskId, task] of Object.entries(tasks)) {
    for (const dep of task.depends_on ?? []) {
      expect(tasks[dep], `${taskId} depends on missing ${dep}`).toBeTruthy()
      outgoing[dep] += 1
    }
  }
  const isolated = Object.entries(tasks)
    .filter(([, task]) => (task.depends_on ?? []).length === 0)
    .map(([taskId]) => taskId)
    .filter((taskId) => (outgoing[taskId] ?? 0) === 0)
  expect(isolated, 'DAG must not contain lone isolated task nodes').toEqual([])
}

function assertBranchBoundaryReset(packagePath: string, taskId: string): void {
  const workflow = readWorkflow(packagePath)
  const tasks = workflow.tasks ?? {}
  expect(tasks[taskId], `branch target ${taskId} must exist`).toBeTruthy()
  const status = tasks[taskId].state?.status
  expect(status, `branch target ${taskId} should be ready before child execution`).toBe(
    'ready',
  )

  const descendants = descendantsOf(tasks, taskId)
  expect(descendants.length, `${taskId} should have downstream branch work`).toBeGreaterThan(0)
  for (const descendant of descendants) {
    expect(
      ['pending', 'ready'].includes(tasks[descendant].state?.status ?? ''),
      `${descendant} must be reset for branch execution`,
    ).toBe(true)
  }
}

function descendantsOf(tasks: Record<string, WorkflowTask>, taskId: string): string[] {
  const reverse: Record<string, string[]> = {}
  for (const [id, task] of Object.entries(tasks)) {
    for (const dep of task.depends_on ?? []) {
      ;(reverse[dep] ||= []).push(id)
    }
  }
  const seen = new Set<string>()
  const queue = [...(reverse[taskId] ?? [])]
  while (queue.length > 0) {
    const current = queue.shift()!
    if (seen.has(current)) continue
    seen.add(current)
    queue.push(...(reverse[current] ?? []))
  }
  return [...seen].sort()
}

function assertSessionFileLineage(
  childSessionId: string,
  parentSessionId: string,
  taskId: string,
): void {
  const candidateDirs = [
    process.env.ECAA_PLAYWRIGHT_SESSIONS_DIR,
    E2E_SESSIONS_DIR,
    process.env.ECAA_CHAT_SESSIONS_DIR,
  ].filter((dir): dir is string => Boolean(dir))
  const sessionPath = candidateDirs
    .map((dir) => join(dir, `${childSessionId}.json`))
    .find((path) => existsSync(path))
  expect(
    sessionPath,
    `child session file in one of ${candidateDirs.join(', ')}`,
  ).toBeTruthy()
  const session = JSON.parse(readFileSync(sessionPath!, 'utf8')) as {
    lineage?: {
      parent_session_id?: string
      branched_from_task_id?: string
    }
  }
  expect(session.lineage?.parent_session_id).toBe(parentSessionId)
  expect(session.lineage?.branched_from_task_id).toBe(taskId)
}

function writeAutoApproveDiscoveries(packagePath: string): void {
  const runtimeDir = join(packagePath, 'runtime')
  mkdirSync(runtimeDir, { recursive: true })
  writeFileSync(
    join(runtimeDir, '.sme-auto-approve-discoveries'),
    JSON.stringify(
      { allow: DEFAULT_AUTO_APPROVE_DISCOVERIES, deny: [] },
      null,
      2,
    ),
  )
}

function assertAllRequiredFiguresRendered(
  packagePath: string,
  figureIdsInUi: string[],
): string[] {
  const expected = requiredFigureIds(packagePath)
  expect(expected.length, 'emitted DAG must declare at least one eligible figure').toBeGreaterThan(0)

  const uiIds = new Set(figureIdsInUi)
  const missingFromUi = expected.filter((id) => !uiIds.has(id))
  expect(
    missingFromUi,
    `Figures tab is missing required figure ids. UI ids: [${figureIdsInUi.join(', ')}]`,
  ).toEqual([])

  for (const [taskId, figureIds] of requiredFiguresByTask(packagePath)) {
    for (const figureId of figureIds) {
      const figuresDir = join(packagePath, 'runtime', 'outputs', taskId, 'figures')
      const png = join(figuresDir, `${figureId}.png`)
      const pdf = join(figuresDir, `${figureId}.pdf`)
      expect(existsSync(png), `${taskId}/${figureId}.png must exist`).toBe(true)
      expect(readFileSync(png).subarray(0, 8).toString('binary')).toBe('\x89PNG\r\n\x1A\n')
      expect(existsSync(pdf), `${taskId}/${figureId}.pdf must exist`).toBe(true)
      expect(readFileSync(pdf).subarray(0, 5).toString()).toBe('%PDF-')
    }
  }
  return expected
}

function requiredFigureIds(packagePath: string): string[] {
  const ids = new Set<string>()
  for (const [, figureIds] of requiredFiguresByTask(packagePath)) {
    for (const id of figureIds) ids.add(id)
  }
  return [...ids].sort()
}

function requiredFiguresByTask(packagePath: string): Array<[string, string[]]> {
  const outputsDir = join(packagePath, 'runtime', 'outputs')
  const pairs: Array<[string, string[]]> = []
  for (const taskId of readdirSync(outputsDir)) {
    const specPath = join(outputsDir, taskId, 'task-spec.json')
    if (!existsSync(specPath)) continue
    const spec = JSON.parse(readFileSync(specPath, 'utf8')) as {
      spec?: { required_figures?: unknown[] }
    }
    const figureIds = (spec.spec?.required_figures ?? [])
      .filter((id): id is string => typeof id === 'string' && id.length > 0)
      .sort()
    if (figureIds.length > 0) pairs.push([taskId, figureIds])
  }
  return pairs.sort(([a], [b]) => a.localeCompare(b))
}

function assertCompletionOrderRespectsDependencies(
  packagePath: string,
  opts: { requireEveryTaskEvent: boolean },
): void {
  const workflow = readWorkflow(packagePath)
  const events = completedTaskEvents(packagePath)
  expect(events.length, 'package LOG.jsonl must include completed task events').toBeGreaterThan(0)
  const eventSet = new Set(events)
  const position = new Map(events.map((taskId, index) => [taskId, index]))
  const taskIds = Object.keys(workflow.tasks ?? {})
  if (opts.requireEveryTaskEvent) {
    expect(eventSet, 'every task must emit one completion event').toEqual(new Set(taskIds))
  }
  for (const [taskId, task] of Object.entries(workflow.tasks ?? {})) {
    if (!position.has(taskId)) continue
    for (const dep of task.depends_on ?? []) {
      if (!position.has(dep)) continue
      expect(
        position.get(dep)!,
        `${taskId} completed before dependency ${dep}; events=${events.join(',')}`,
      ).toBeLessThan(position.get(taskId)!)
    }
  }
}

function completedTaskEvents(packagePath: string): string[] {
  const logPath = join(packagePath, 'runtime', 'LOG.jsonl')
  expect(existsSync(logPath), `${logPath} must exist`).toBe(true)
  return readFileSync(logPath, 'utf8')
    .split(/\r?\n/)
    .filter((line) => line.trim().length > 0)
    .map((line) => JSON.parse(line) as { event?: string; task?: string })
    .filter((event) => event.event === 'completed' && typeof event.task === 'string')
    .map((event) => event.task!)
}

function assertExecutedTasksHaveContainerEvidence(
  packagePath: string,
  expectedImage: string,
): void {
  const outputsDir = join(packagePath, 'runtime', 'outputs')
  const evidence: string[] = []
  for (const taskId of readdirSync(outputsDir)) {
    const statePath = join(outputsDir, taskId, '.container-state.json')
    if (!existsSync(statePath)) continue
    evidence.push(taskId)
    const state = JSON.parse(readFileSync(statePath, 'utf8')) as {
      runtime?: string
      image?: string
      exit_code?: number
      task_id?: string
    }
    expect(state.runtime, `${taskId} runtime`).toBe('docker')
    expect(state.image, `${taskId} image`).toBe(expectedImage)
    expect(state.exit_code, `${taskId} exit code`).toBe(0)
    expect(state.task_id, `${taskId} sidecar task id`).toBe(taskId)
  }
  expect(evidence.length, 'branch package must include container evidence').toBeGreaterThan(0)
}

function ensurePackageGitCommitted(packagePath: string, message: string): void {
  if (!existsSync(join(packagePath, '.git'))) {
    runGit(packagePath, ['init'])
  }
  runGit(packagePath, ['config', 'user.name', 'Scripps Live QA'])
  runGit(packagePath, ['config', 'user.email', 'live-qa@scripps.local'])
  runGit(packagePath, ['add', '-A'])
  const diff = spawnSync('git', ['-C', packagePath, 'diff', '--cached', '--quiet'])
  if (diff.status !== 0) {
    runGit(packagePath, ['commit', '-m', message])
  }
  const status = spawnSync('git', ['-C', packagePath, 'status', '--porcelain'], {
    encoding: 'utf8',
  })
  expect(status.status, status.stderr).toBe(0)
  expect(status.stdout.trim(), 'analysis package git tree must be clean').toBe('')
}

function runGit(packagePath: string, args: string[]): void {
  const result = spawnSync('git', ['-C', packagePath, ...args], {
    encoding: 'utf8',
  })
  expect(
    result.status,
    `git ${args.join(' ')} failed in ${packagePath}\n${result.stdout}\n${result.stderr}`,
  ).toBe(0)
}

function readWorkflow(packagePath: string): Workflow {
  return JSON.parse(readFileSync(join(packagePath, 'WORKFLOW.json'), 'utf8')) as Workflow
}

interface Workflow {
  tasks?: Record<string, WorkflowTask>
}

interface WorkflowTask {
  state?: { status?: string }
  depends_on?: string[]
}

async function captureScreenshot(page: Page, path: string): Promise<void> {
  await page.screenshot({ path, fullPage: true, timeout: 60_000 }).catch(async () => {
    await page.screenshot({ path, fullPage: false, timeout: 30_000 })
  })
}

async function expectNoDocumentHorizontalScroll(page: Page): Promise<void> {
  await expect
    .poll(() => page.evaluate(() => window.scrollX), { timeout: 5_000 })
    .toBe(0)
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}
