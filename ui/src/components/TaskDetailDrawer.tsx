/**
 * TaskDetailDrawer — the SME's single surface for everything about one
 * task in the plan.
 *
 * Sections:
 *  1. Header with the friendly stage name + current status
 *  2. "What this step does" — plain-English copy from stage-descriptions.yaml
 *  (falls back to task.description if the stage_class has no entry)
 *  3. Method currently chosen (from session.intake_methods / task.spec)
 *  4. Upstream requirements — what must finish before this runs
 *  5. Downstream impact — forward-slice preview; what re-runs if this changes
 *  6. Results — clickable figure thumbnails (reuses /artifacts/*path)
 *  7. Activity log — tail of progress.log (polled every 2 s when running)
 *  8. Decision trail — entries from /decisions filtered to this stage
 *  9. Action footer: Change method / Rerun / Branch
 *
 * Each mutation opens a confirm-modal that first fetches an impact
 * preview so the SME sees the blast radius + cost range before committing.
 *
 * Keyboard: Escape closes. Action buttons are tabbable. Active-task
 * pulse outline on the DagCanvas node is driven by the `activeTaskId`
 * state lifted in PlanTab.
 */

import { useEffect, useMemo, useRef, useState } from 'react'
import { useCancelableEffect } from '../hooks/useCancelableFetch'
import type { DAG, Task } from '../types'
import type {
  ImpactPreviewResponse,
  ProgressLogResponse,
  StageDescription,
  TaskResultPayload,
  DecisionsResponse,
} from '../api/chatClient'
import {
  getDecisions,
  getProgressLog,
  getStageDescriptions,
  getTaskResult,
  postAmendMethod,
  postBranch,
  postImpactPreview,
  postRerun,
  postTaskNote,
  postUndoAmendment,
  artifactUrl,
  unblockChatSession,
  verifyTask,
} from '../api/chatClient'
import type { ClaimVerificationReport } from '../types/ClaimVerificationReport'
import type { ClaimVerdict } from '../types/ClaimVerdict'
import { useUndoStack } from '../hooks/useUndoStack'
import { LOG_POLL_MS } from '../lib/polling'
import { relativeTime } from '../lib/time'
import { Z } from '../lib/z-index'
import { Dialog } from './primitives/Dialog'
import { formatUSD } from '../lib/format'
import BlockerCard from './BlockerCard'
import ExplainButton from './ExplainButton'
import TaskOpsPanel from './TaskOpsPanel'
import AgentCodeSubtab from './AgentCodeSubtab'

interface Props {
  sessionId: string | null
  taskId: string | null
  dag: DAG | null
  onClose: () => void
  /** Navigate to another task when the user clicks an upstream/downstream link. */
  onTaskLink?: (taskId: string) => void
}

const FRIENDLY_STATUS: Record<string, { label: string; color: string; icon: string }> = {
  pending:   { label: 'Waiting on an upstream step',   color: 'var(--color-text-muted)', icon: '⏸' },
  ready:     { label: 'Ready to run',                   color: 'var(--color-accent)', icon: '⏱' },
  running:   { label: 'Currently running',              color: 'var(--color-warning-accent)', icon: '▶' },
  completed: { label: 'Finished',                       color: 'var(--color-success-accent)', icon: '✓' },
  failed:    { label: 'Failed — needs attention',       color: 'var(--color-danger-accent)', icon: '✗' },
  blocked:   { label: 'Needs your input',               color: 'var(--color-warning-accent)', icon: '⚠' },
}

const BTN_PRIMARY: React.CSSProperties = {
  padding: '0.55rem 0.95rem',
  borderRadius: 6,
  border: '1px solid #2563eb',
  background: 'var(--color-accent)',
  color: 'white',
  fontWeight: 600,
  fontSize: '0.82rem',
  cursor: 'pointer',
}
const BTN_SECONDARY: React.CSSProperties = {
  padding: '0.55rem 0.95rem',
  borderRadius: 6,
  border: '1px solid var(--color-border-strong)',
  background: 'var(--color-surface-1)',
  color: 'var(--color-text-secondary)',
  fontWeight: 500,
  fontSize: '0.82rem',
  cursor: 'pointer',
}
const BTN_DANGER_OUTLINE: React.CSSProperties = {
  ...BTN_SECONDARY,
  borderColor: 'var(--color-danger-accent)',
  color: 'var(--color-danger-fg)',
}

interface ConfirmModal {
  kind: 'amend' | 'rerun' | 'branch'
  title: string
  prompt: string
  rationale: string
  newMethod?: string
  preview: ImpactPreviewResponse | null
  previewError?: string
  busy: boolean
  error?: string
}

export default function TaskDetailDrawer({
  sessionId,
  taskId,
  dag,
  onClose,
  onTaskLink,
}: Props): JSX.Element | null {
  const [descriptions, setDescriptions] = useState<Record<string, StageDescription>>({})
  const [descFetched, setDescFetched] = useState(false)
  const [taskResult, setTaskResult] = useState<TaskResultPayload | null>(null)
  const [logData, setLogData] = useState<ProgressLogResponse | null>(null)
  const [decisions, setDecisions] = useState<DecisionsResponse['decisions']>([])
  const [lightbox, setLightbox] = useState<string | null>(null)
  const [modal, setModal] = useState<ConfirmModal | null>(null)
  const logSinceRef = useRef(0)
  const undoStack = useUndoStack()
  const [newNote, setNewNote] = useState('')
  const [notePosting, setNotePosting] = useState(false)
  const [noteErr, setNoteErr] = useState<string | null>(null)

  const task: Task | null = useMemo(() => {
    if (!dag || !taskId) return null
    return dag.tasks[taskId] ?? null
  }, [dag, taskId])

  // ── stage-descriptions load (once per tab) ──────────────────────────────
  useCancelableEffect(async ({ cancelled }) => {
    if (descFetched) return
    try {
      const res = await getStageDescriptions()
      if (!cancelled()) {
        setDescriptions(res.stages ?? {})
        setDescFetched(true)
      }
    } catch {
      if (!cancelled()) setDescFetched(true) // don't retry on error
    }
  }, [descFetched])

  // ── task-result + decisions load on task change ─────────────────────────
  useCancelableEffect(async ({ cancelled }) => {
    if (!sessionId || !taskId) {
      setTaskResult(null)
      setDecisions([])
      logSinceRef.current = 0
      setLogData(null)
      return
    }
    try {
      const r = await getTaskResult(sessionId, taskId)
      if (!cancelled()) setTaskResult(r)
    } catch {
      if (!cancelled()) setTaskResult(null)
    }
    try {
      const d = await getDecisions(sessionId)
      // The agent (claude subprocess) sometimes appends free-form
      // audit entries to runtime/decisions.jsonl that don't match the
      // typed DecisionRecord shape (e.g., `{kind:"discovery_auto_pick",
      // Task_id:"...",...}` with `kind` at the top level instead of
      // nested under `decision.kind`). Drop those before any access
      // to `.decision.kind` so the drawer doesn't crash with
      // "can't access property kind, decision is undefined".
      if (!cancelled()) {
        setDecisions(
          d.decisions.filter(
            (rec) =>
              rec &&
              rec.decision &&
              typeof rec.decision.kind === 'string',
          ),
        )
      }
    } catch {
      if (!cancelled()) setDecisions([])
    }
  }, [sessionId, taskId])

  // ── progress.log poll (2 s) while task is running ───────────────────────
  useEffect(() => {
    if (!sessionId || !taskId) return
    logSinceRef.current = 0
    let cancelled = false
    let timer: number | null = null
    const tick = async () => {
      try {
        const data = await getProgressLog(sessionId, taskId, logSinceRef.current)
        if (cancelled) return
        setLogData((prev) => {
          if (!prev) return data
          // Append new lines; cap at 500 lines so the drawer doesn't
          // grow unboundedly.
          const merged = [...prev.lines, ...data.lines].slice(-500)
          return {
            ...data,
            lines: merged,
          }
        })
        logSinceRef.current = data.next_since_line
      } catch {
        // swallow — temporary network blip shouldn't break the drawer
      }
      if (!cancelled) {
        timer = window.setTimeout(tick, LOG_POLL_MS)
      }
    }
    void tick()
    return () => {
      cancelled = true
      if (timer !== null) clearTimeout(timer)
    }
  }, [sessionId, taskId])

  // ── Escape key closes drawer; ArrowEscape closes lightbox first ─────────
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return
      if (lightbox) {
        setLightbox(null)
        return
      }
      if (modal) {
        setModal(null)
        return
      }
      onClose()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose, lightbox, modal])

  // ── downstream impact (forward slice from dag, client-side) ─────────────
  //
  // Hook must be called unconditionally on every render to satisfy React's
  // rules of hooks — the early `if (!taskId || !task) return null` below
  // would otherwise skip this hook when the drawer is closed and crash on
  // reopen with "Rendered more hooks than during the previous render".
  const downstreamIds = useMemo(() => {
    if (!dag || !taskId) return []
    return computeForwardSlice(dag, taskId).filter((t) => t !== taskId)
  }, [dag, taskId])

  // ── fetch impact preview lazily after the modal opens ───────────────────
  // Same rules-of-hooks concern — kept above the early return, guarded by
  // the null-check at the top of the effect body.
  useCancelableEffect(async ({ cancelled }) => {
    if (!modal || modal.preview || !sessionId || !taskId) return
    try {
      const preview = await postImpactPreview(
        sessionId,
        taskId,
        modal.kind === 'amend' ? modal.newMethod || undefined : undefined,
      )
      if (!cancelled()) setModal((prev) => (prev ? { ...prev, preview } : prev))
    } catch (e) {
      if (!cancelled())
        setModal((prev) =>
          prev ? { ...prev, previewError: (e as Error).message } : prev,
        )
    }
  }, [modal, sessionId, taskId])

  if (!taskId || !task) return null

  const statusInfo = FRIENDLY_STATUS[task.state.status] ?? {
    label: task.state.status,
    color: 'var(--color-text-muted)',
    icon: '•',
  }
  const stageClass = typeof task.spec === 'object' && task.spec
    ? ((task.spec as Record<string, unknown>)['stage_class'] as string | undefined)
    : undefined
  const desc: StageDescription | undefined = stageClass ? descriptions[stageClass] : undefined
  const friendlyName = desc?.sme_friendly_name ?? prettifyTaskId(taskId)
  // Two distinct copy sources:
  // 1. task.description — the per-task text the DAG node shows
  // (truncated with ellipsis on the TaskCard). The SME sees the
  // truncated form on the node then clicks; they expect to see the
  // full version in the drawer, so always surface it.
  // 2. stageDescription — generic plain-English category text from
  // config/stage-descriptions.yaml. Optional; rendered alongside
  // the task-specific text when present + different.
  const taskDescription = task.description?.trim() ?? ''
  const stageDescription = (desc?.long ?? desc?.short ?? '').trim()
  const stageDescriptionIsDistinct =
    stageDescription.length > 0 && stageDescription !== taskDescription

  const upstreamIds = task.depends_on ?? []

  // ── method currently chosen (best-effort from task result / spec) ───────
  const currentMethod: string | null = (() => {
    if (!taskResult) return null
    const result = (taskResult.result ?? {}) as Record<string, unknown>
    const candidates = [
      result['chosen_method'],
      result['method'],
      result['final_chosen_method'],
    ]
    for (const c of candidates) {
      if (typeof c === 'string' && c.trim().length > 0) return c
    }
    return null
  })()

  // ── decisions filtered to this task/stage ───────────────────────────────
  const myDecisions = decisions.filter((d) => {
    const k = d.decision.kind
    const stage = (d.decision['stage'] ?? d.decision['target_stage']) as string | undefined
    const tid = d.decision['task_id'] as string | undefined
    if (stage && stage === taskId) return true
    if (tid && tid === taskId) return true
    // confirm / emit / branch etc. are session-level; drop from per-task view.
    void k
    return false
  })

  // ── actions ─────────────────────────────────────────────────────────────
  const openAmendModal = () => {
    setModal({
      kind: 'amend',
      title: `Change the method for ${friendlyName}`,
      prompt:
        'Type the new method name (e.g. a candidate from an earlier approval gate, or a free-text method description).',
      rationale: `Replacing method for ${friendlyName} — `,
      newMethod: '',
      preview: null,
      busy: false,
    })
  }
  const openRerunModal = () => {
    setModal({
      kind: 'rerun',
      title: `Rerun ${friendlyName} with its current method`,
      prompt:
        'Reruns this step and every step that depends on it. The method stays the same.',
      rationale: `Rerunning ${friendlyName} — `,
      preview: null,
      busy: false,
    })
  }
  const openBranchModal = () => {
    setModal({
      kind: 'branch',
      title: 'Explore this change in a branch (keep the current analysis intact)',
      prompt:
        'Creates a copy of this session so you can try a different choice without throwing away the current results.',
      rationale: `Branching to try an alternative for ${friendlyName} — `,
      preview: null,
      busy: false,
    })
  }

  const submitAmend = async () => {
    if (!modal || !sessionId || !taskId || !modal.newMethod?.trim()) return
    setModal({ ...modal, busy: true, error: undefined })
    try {
      const res = await postAmendMethod(
        sessionId,
        taskId,
        modal.newMethod.trim(),
        modal.rationale,
      )
      // Surface an Undo toast with the prior prose for 30s. Only push
      // a token when there *was* a prior method (empty string =
      // first-time authoring, nothing to undo back to).
      const prior = res.prior_method_prose ?? ''
      const sid = sessionId
      const tid = taskId
      if (prior.trim() !== '') {
        undoStack.push({
          kind: 'amend',
          label: 'Method amended',
          undo: async () => {
            await postUndoAmendment(sid, tid, prior)
          },
        })
      }
      setModal(null)
      onClose()
    } catch (e) {
      setModal((prev) =>
        prev ? { ...prev, busy: false, error: (e as Error).message } : prev,
      )
    }
  }
  const submitRerun = async () => {
    if (!modal || !sessionId || !taskId) return
    setModal({ ...modal, busy: true, error: undefined })
    try {
      await postRerun(sessionId, taskId, modal.rationale)
      setModal(null)
      onClose()
    } catch (e) {
      setModal((prev) =>
        prev ? { ...prev, busy: false, error: (e as Error).message } : prev,
      )
    }
  }
  const submitBranch = async () => {
    if (!modal || !sessionId) return
    setModal({ ...modal, busy: true, error: undefined })
    try {
      const body = await postBranch(sessionId, {
        rationale: modal.rationale || undefined,
        taskId: taskId ?? undefined,
      })
      const childId = body.session_id
      setModal(null)
      const url = `/?session=${childId}&branched_from=${sessionId}`
      window.location.href = url
    } catch (e) {
      setModal((prev) =>
        prev ? { ...prev, busy: false, error: (e as Error).message } : prev,
      )
    }
  }

  // ── render ──────────────────────────────────────────────────────────────
  return (
    <aside
      role="dialog"
      aria-label={`Details for ${friendlyName}`}
      data-testid="task-detail-drawer"
      style={drawerStyle}
    >
      <header style={headerStyle}>
        <div style={{ display: 'flex', flexDirection: 'column', gap: 4, minWidth: 0 }}>
          <div
            style={{
              fontSize: '0.72rem',
              color: 'var(--color-text-muted)',
              textTransform: 'uppercase',
              letterSpacing: '0.05em',
              overflow: 'hidden',
              textOverflow: 'ellipsis',
              whiteSpace: 'nowrap',
            }}
          >
            {taskId}
          </div>
          <h2 style={{ margin: 0, fontSize: '1.12rem', color: 'var(--color-text-primary)' }}>
            {friendlyName}
          </h2>
          <div style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
            <span aria-hidden style={{ fontSize: '0.9rem' }}>
              {statusInfo.icon}
            </span>
            <span style={{ color: statusInfo.color, fontSize: '0.8rem', fontWeight: 500 }}>
              {statusInfo.label}
            </span>
          </div>
        </div>
        <button
          type="button"
          aria-label="Close"
          onClick={onClose}
          style={{
            border: 'none',
            background: 'transparent',
            fontSize: '1.25rem',
            color: 'var(--color-text-muted)',
            cursor: 'pointer',
            padding: 4,
            lineHeight: 1,
          }}
        >
          ×
        </button>
      </header>

      <div style={bodyStyle}>
        {task.state.status === 'blocked' && sessionId && taskId && (
          <Section title="Resolve this blocker">
            <BlockerCard
              reason={
                (task.state as { record?: { reason?: string } }).record?.reason ??
                'This task is blocked. Choose how to proceed.'
              }
              recoveryHint=""
              onUnblock={async () => {
                if (!sessionId) return
                await unblockChatSession(sessionId)
              }}
              sessionId={sessionId}
              taskId={taskId}
            />
          </Section>
        )}

        {taskDescription.length > 0 && (
          <Section title="This specific task">
            <p
              style={{
                margin: 0,
                color: 'var(--color-text-secondary)',
                whiteSpace: 'pre-line',
                lineHeight: 1.5,
              }}
            >
              {taskDescription}
              {taskDescription.length > 120 && (
                <ExplainButton text={taskDescription} context="task description" />
              )}
            </p>
          </Section>
        )}

        {(stageDescriptionIsDistinct || desc?.example_outputs) && (
          <Section
            title={
              taskDescription.length > 0
                ? 'About this kind of step'
                : 'What this step does'
            }
          >
            {stageDescriptionIsDistinct && (
              <p
                style={{
                  margin: 0,
                  color: 'var(--color-text-secondary)',
                  whiteSpace: 'pre-line',
                  lineHeight: 1.5,
                }}
              >
                {stageDescription}
              </p>
            )}
            {desc?.example_outputs && (
              <div style={{ marginTop: 8, fontSize: '0.82rem', color: 'var(--color-text-muted)' }}>
                <strong style={{ color: 'var(--color-text-secondary)' }}>Produces:</strong>{' '}
                {desc.example_outputs}
              </div>
            )}
          </Section>
        )}

        {currentMethod && (
          <Section title="Method currently chosen">
            <code
              style={{
                fontSize: '0.88rem',
                background: 'var(--color-border-subtle)',
                padding: '0.25rem 0.5rem',
                borderRadius: 4,
                wordBreak: 'break-word',
              }}
            >
              {currentMethod}
            </code>
          </Section>
        )}

        {upstreamIds.length > 0 && (
          <Section title="Needs these to finish first">
            <ul style={listStyle}>
              {upstreamIds.map((u) => {
                const upTask = dag?.tasks[u]
                const upStatus = upTask?.state.status ?? 'pending'
                const upInfo = FRIENDLY_STATUS[upStatus]
                return (
                  <li key={u} style={listItemStyle}>
                    <button
                      type="button"
                      onClick={() => onTaskLink?.(u)}
                      style={linkButtonStyle}
                    >
                      <span aria-hidden style={{ color: upInfo?.color }}>
                        {upInfo?.icon ?? '•'}
                      </span>
                      <span>{describeTask(upTask, u, descriptions)}</span>
                    </button>
                  </li>
                )
              })}
            </ul>
          </Section>
        )}

        {downstreamIds.length > 0 && (
          <Section title={`If this changes, these ${downstreamIds.length} steps will re-run`}>
            <ul style={listStyle}>
              {downstreamIds.map((d) => (
                <li key={d} style={listItemStyle}>
                  <button
                    type="button"
                    onClick={() => onTaskLink?.(d)}
                    style={linkButtonStyle}
                  >
                    <span aria-hidden style={{ color: 'var(--color-text-faint)' }}>↳</span>
                    <span>{describeTask(dag?.tasks[d], d, descriptions)}</span>
                  </button>
                </li>
              ))}
            </ul>
          </Section>
        )}

        {taskResult && taskResult.artifacts && taskResult.artifacts.length > 0 && (
          <Section title="Results">
            <div
              style={{
                display: 'grid',
                gridTemplateColumns: 'repeat(auto-fill, minmax(140px, 1fr))',
                gap: 8,
              }}
            >
              {taskResult.artifacts
                .filter((a) => a.mime_type.startsWith('image/'))
                .map((a) => {
                  const url = sessionId ? artifactUrl(sessionId, a.relative_path) : ''
                  return (
                    <button
                      key={a.name}
                      type="button"
                      onClick={() => setLightbox(url)}
                      aria-label={`Open ${a.name}`}
                      style={thumbButtonStyle}
                    >
                      <img
                        src={url}
                        alt={a.name}
                        style={{
                          width: '100%',
                          display: 'block',
                          borderRadius: 4,
                          background: 'var(--color-surface-0)',
                        }}
                      />
                      <div style={thumbLabelStyle}>{a.name}</div>
                    </button>
                  )
                })}
            </div>
          </Section>
        )}

        {taskResult?.verification && sessionId && taskId && (
          <Section title="Claim verification">
            <ClaimVerificationPanel
              sessionId={sessionId}
              taskId={taskId}
              initialReport={taskResult.verification}
            />
          </Section>
        )}

        {logData && logData.lines.length > 0 && (
          <Section
            title={`Activity log${logData.truncated ? ' (recent lines only)' : ''}`}
          >
            <pre style={logPreStyle}>{logData.lines.join('\n')}</pre>
          </Section>
        )}

        {sessionId && taskId && (
          <Section title="Wrapper status / Logs / Scripts">
            <TaskOpsPanel
              sessionId={sessionId}
              taskId={taskId}
              isRunning={task?.state.status === 'running'}
            />
          </Section>
        )}

        {(taskResult?.agent_code != null || task.state.status === 'completed') && (
          <Section title="Code">
            <AgentCodeSubtab agentCode={taskResult?.agent_code} />
          </Section>
        )}

        {myDecisions.length > 0 && (
          <Section title="Decision trail">
            <ul style={{ ...listStyle, gap: 6 }}>
              {myDecisions.map((d, i) => (
                <li key={i} style={decisionItemStyle}>
                  <div
                    style={{ fontSize: '0.72rem', color: 'var(--color-text-muted)' }}
                    title={new Date(d.timestamp).toLocaleString()}
                  >
                    {relativeTime(d.timestamp)} • {d.actor}
                  </div>
                  <div style={{ fontSize: '0.85rem', color: 'var(--color-text-primary)' }}>
                    <strong>{decisionLabel(d.decision.kind)}</strong>
                    {d.decision.kind === 'user_note' && (
                      <>
                        {' — '}
                        <span style={{ color: 'var(--color-text-secondary)' }}>
                          {(d.decision as { body?: string }).body ?? ''}
                        </span>
                      </>
                    )}
                    {d.rationale && d.decision.kind !== 'user_note' && (
                      <>
                        {' — '}
                        <span style={{ color: 'var(--color-text-secondary)' }}>{d.rationale}</span>
                      </>
                    )}
                  </div>
                </li>
              ))}
            </ul>
          </Section>
        )}

        <Section title="Notes">
          <textarea
            value={newNote}
            onChange={(e) => setNewNote(e.target.value)}
            placeholder="Add a note for yourself or collaborators…"
            rows={3}
            style={{
              width: '100%',
              padding: '0.5rem 0.6rem',
              border: '1px solid var(--color-border-default)',
              borderRadius: 4,
              fontFamily: 'inherit',
              fontSize: '0.85rem',
              color: 'var(--color-text-primary)',
              background: 'var(--color-surface-0)',
              resize: 'vertical',
            }}
          />
          {noteErr && (
            <div
              role="alert"
              style={{
                marginTop: 4,
                color: 'var(--color-danger-accent)',
                fontSize: '0.75rem',
              }}
            >
              {noteErr}
            </div>
          )}
          <div style={{ marginTop: 6, display: 'flex', justifyContent: 'flex-end' }}>
            <button
              type="button"
              disabled={notePosting || !newNote.trim() || !sessionId || !taskId}
              onClick={async () => {
                if (!sessionId || !taskId) return
                setNotePosting(true)
                setNoteErr(null)
                try {
                  await postTaskNote(sessionId, taskId, newNote.trim())
                  setNewNote('')
                  // Refresh the decisions list so the new note appears.
                  try {
                    const res = await getDecisions(sessionId)
                    // Same defensive filter as the initial load — drop
                    // any agent-appended free-form entries that lack
                    // the typed `decision.kind` shape.
                    setDecisions(
                      res.decisions.filter(
                        (rec) =>
                          rec &&
                          rec.decision &&
                          typeof rec.decision.kind === 'string',
                      ),
                    )
                  } catch {
                    // Non-fatal.
                  }
                } catch (e) {
                  setNoteErr(e instanceof Error ? e.message : String(e))
                } finally {
                  setNotePosting(false)
                }
              }}
              style={BTN_SECONDARY}
            >
              {notePosting ? 'Saving…' : 'Add note'}
            </button>
          </div>
        </Section>
      </div>

      <footer style={footerStyle}>
        <button
          type="button"
          onClick={openAmendModal}
          style={BTN_PRIMARY}
          disabled={task.state.status !== 'completed'}
          title={
            task.state.status !== 'completed'
              ? 'This step has not finished yet — amend is available after it completes'
              : 'Change the method this step uses'
          }
        >
          Change method
        </button>
        <button
          type="button"
          onClick={openRerunModal}
          style={BTN_SECONDARY}
          disabled={task.state.status !== 'completed'}
          title={
            task.state.status !== 'completed'
              ? 'This step has not finished yet — rerun is available after it completes'
              : 'Rerun this step with the same method'
          }
        >
          Rerun
        </button>
        <button
          type="button"
          onClick={openBranchModal}
          style={BTN_DANGER_OUTLINE}
          title="Try an alternative without changing the current session"
        >
          Explore in a branch
        </button>
      </footer>

      {lightbox && (
        <div
          role="dialog"
          aria-label="Figure preview"
          onClick={() => setLightbox(null)}
          style={lightboxBackdrop}
        >
          <img
            src={lightbox}
            alt="preview"
            style={{ maxWidth: '92vw', maxHeight: '92vh', borderRadius: 6 }}
          />
        </div>
      )}

      {modal && (
        <ConfirmModalView
          modal={modal}
          setModal={setModal}
          onAmend={submitAmend}
          onRerun={submitRerun}
          onBranch={submitBranch}
        />
      )}
    </aside>
  )
}

// ── ConfirmModalView ──────────────────────────────────────────────────────

interface ConfirmModalViewProps {
  modal: ConfirmModal
  setModal: React.Dispatch<React.SetStateAction<ConfirmModal | null>>
  onAmend: () => Promise<void>
  onRerun: () => Promise<void>
  onBranch: () => Promise<void>
}

function ConfirmModalView({
  modal,
  setModal,
  onAmend,
  onRerun,
  onBranch,
}: ConfirmModalViewProps): JSX.Element {
  const submit = () => {
    if (modal.kind === 'amend') void onAmend()
    else if (modal.kind === 'rerun') void onRerun()
    else void onBranch()
  }
  const canSubmit =
    !modal.busy &&
    modal.rationale.trim().length > 0 &&
    (modal.kind !== 'amend' || (modal.newMethod ?? '').trim().length > 0)
  const costRange = modal.preview
    ? `${formatUSD(modal.preview.est_cost_usd_min)} – ${formatUSD(modal.preview.est_cost_usd_max)}`
    : null
  return (
    <Dialog
      onClose={() => {
        if (!modal.busy) setModal(null)
      }}
      ariaLabel={modal.title}
      closeOnOutsideClick={!modal.busy}
      zIndex={Z.NESTED_DRAWER}
      contentStyle={modalCardStyle}
    >
        <h3 style={{ margin: '0 0 8px 0', fontSize: '1.02rem', color: 'var(--color-text-primary)' }}>
          {modal.title}
        </h3>
        <p style={{ margin: '0 0 14px 0', color: 'var(--color-text-secondary)', fontSize: '0.87rem', lineHeight: 1.5 }}>
          {modal.prompt}
        </p>

        {modal.kind === 'amend' && (
          <label style={{ display: 'block', marginBottom: 10 }}>
            <div style={modalLabelStyle}>New method</div>
            <input
              type="text"
              value={modal.newMethod ?? ''}
              onChange={(e) => setModal({ ...modal, newMethod: e.target.value, preview: null })}
              placeholder="e.g. two_stage_denovo_with_fibrochondrocyte_pericyte"
              style={modalInputStyle}
              autoFocus
            />
          </label>
        )}

        <label style={{ display: 'block', marginBottom: 12 }}>
          <div style={modalLabelStyle}>Reason (shown in the decision log)</div>
          <textarea
            value={modal.rationale}
            onChange={(e) => setModal({ ...modal, rationale: e.target.value })}
            rows={3}
            style={{ ...modalInputStyle, fontFamily: 'inherit', resize: 'vertical' }}
            placeholder="A short note helps future-you (and auditors) understand why you made this change."
          />
        </label>

        {modal.kind !== 'branch' && (
          <div style={previewBoxStyle}>
            {modal.previewError && (
              <div style={{ color: 'var(--color-danger-fg)', fontSize: '0.82rem' }}>
                {modal.previewError}
              </div>
            )}
            {!modal.preview && !modal.previewError && (
              <div style={{ color: 'var(--color-text-muted)', fontSize: '0.82rem' }}>Calculating impact…</div>
            )}
            {modal.preview && (
              <>
                <div style={{ fontSize: '0.85rem', color: 'var(--color-text-secondary)', fontWeight: 500 }}>
                  This will re-run {modal.preview.invalidated_count}{' '}
                  step{modal.preview.invalidated_count === 1 ? '' : 's'}.
                </div>
                {costRange && (
                  <div style={{ fontSize: '0.78rem', color: 'var(--color-text-muted)', marginTop: 4 }}>
                    Estimated cost: {costRange}
                  </div>
                )}
                <ul
                  style={{
                    ...listStyle,
                    marginTop: 8,
                    maxHeight: 160,
                    overflow: 'auto',
                  }}
                >
                  {modal.preview.invalidated_tasks.slice(0, 20).map((t) => (
                    <li key={t.task_id} style={{ fontSize: '0.8rem', color: 'var(--color-text-secondary)' }}>
                      • {t.description || t.task_id}
                    </li>
                  ))}
                  {modal.preview.invalidated_tasks.length > 20 && (
                    <li style={{ fontSize: '0.78rem', color: 'var(--color-text-faint)' }}>
                      …and {modal.preview.invalidated_tasks.length - 20} more
                    </li>
                  )}
                </ul>
              </>
            )}
          </div>
        )}

        {modal.error && (
          <div style={{ color: 'var(--color-danger-fg)', fontSize: '0.82rem', marginTop: 8 }}>
            {modal.error}
          </div>
        )}

        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 8, marginTop: 16 }}>
          <button
            type="button"
            onClick={() => setModal(null)}
            disabled={modal.busy}
            style={BTN_SECONDARY}
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={submit}
            disabled={!canSubmit}
            style={{
              ...BTN_PRIMARY,
              opacity: canSubmit ? 1 : 0.55,
              cursor: canSubmit ? 'pointer' : 'not-allowed',
            }}
          >
            {modal.busy
              ? 'Applying…'
              : modal.kind === 'amend'
                ? 'Apply amendment'
                : modal.kind === 'rerun'
                  ? 'Rerun now'
                  : 'Create branch'}
          </button>
        </div>
    </Dialog>
  )
}

// ── helpers ────────────────────────────────────────────────────────────────

function computeForwardSlice(dag: DAG, target: string): string[] {
  const rev: Record<string, string[]> = {}
  for (const [id, t] of Object.entries(dag.tasks)) {
    if (!t) continue
    for (const dep of t.depends_on ?? []) {
      (rev[dep] ||= []).push(id)
    }
  }
  const seen = new Set<string>([target])
  const queue: string[] = [target]
  while (queue.length) {
    const cur = queue.shift()!
    for (const child of rev[cur] ?? []) {
      if (!seen.has(child)) {
        seen.add(child)
        queue.push(child)
      }
    }
  }
  return Array.from(seen).sort()
}

function describeTask(
  task: Task | undefined,
  id: string,
  descriptions: Record<string, StageDescription>,
): string {
  if (!task) return id
  const stageClass =
    typeof task.spec === 'object' && task.spec
      ? ((task.spec as Record<string, unknown>)['stage_class'] as string | undefined)
      : undefined
  const friendly = stageClass ? descriptions[stageClass]?.sme_friendly_name : undefined
  return friendly ?? prettifyTaskId(id)
}

function prettifyTaskId(id: string): string {
  return id
    .replace(/^discover_/, 'Plan: ')
    .replace(/^validate_/, 'Validate: ')
    .replace(/_/g, ' ')
    .replace(/^./, (c) => c.toUpperCase())
}

function decisionLabel(kind: string): string {
  switch (kind) {
    case 'confirm':
      return 'Plan confirmed'
    case 'reject':
      return 'Plan rejected'
    case 'unblock':
      return 'Unblocked'
    case 'branch':
      return 'Branched'
    case 'emit_package':
      return 'Package emitted'
    case 'amend_stage':
      return 'Method changed'
    case 'rerun_task':
      return 'Task reran'
    case 'post_hoc_deviation':
      return 'Post-hoc deviation'
    case 'select_sensitivity_winner':
      return 'Picked sensitivity winner'
    case 'cross_version_diff':
      return 'Cross-version diff'
    case 'auto_advanced':
      return 'Auto-advanced'
    case 'undone_amendment':
      return 'Amendment undone'
    case 'budget_changed':
      return 'Budget changed'
    case 'user_note':
      return 'Note'
    default:
      return kind
  }
}

// ── styles ─────────────────────────────────────────────────────────────────

const drawerStyle: React.CSSProperties = {
  position: 'fixed',
  top: 0,
  right: 0,
  bottom: 0,
  width: 'min(520px, 95vw)',
  background: 'var(--color-surface-1)',
  color: 'var(--color-text-primary)',
  boxShadow: '-6px 0 24px rgba(15, 23, 42, 0.14)',
  display: 'flex',
  flexDirection: 'column',
  zIndex: Z.DROPDOWN,
  borderLeft: '1px solid var(--color-border-default)',
}
const headerStyle: React.CSSProperties = {
  display: 'flex',
  alignItems: 'flex-start',
  justifyContent: 'space-between',
  gap: 12,
  padding: '14px 18px 10px 18px',
  borderBottom: '1px solid var(--color-border-default)',
}
const bodyStyle: React.CSSProperties = {
  flex: 1,
  overflowY: 'auto',
  padding: '12px 18px 16px 18px',
  display: 'flex',
  flexDirection: 'column',
  gap: 18,
}
const footerStyle: React.CSSProperties = {
  display: 'flex',
  gap: 8,
  padding: '12px 18px',
  borderTop: '1px solid var(--color-border-default)',
  background: 'var(--color-surface-0)',
  justifyContent: 'flex-end',
  flexWrap: 'wrap',
}
const sectionStyle: React.CSSProperties = {
  display: 'flex',
  flexDirection: 'column',
  gap: 6,
}
const sectionTitleStyle: React.CSSProperties = {
  fontSize: '0.74rem',
  textTransform: 'uppercase',
  letterSpacing: '0.06em',
  color: 'var(--color-text-faint)',
  fontWeight: 600,
}
const listStyle: React.CSSProperties = {
  margin: 0,
  padding: 0,
  listStyle: 'none',
  display: 'flex',
  flexDirection: 'column',
  gap: 2,
}
const listItemStyle: React.CSSProperties = {
  padding: 0,
}
const linkButtonStyle: React.CSSProperties = {
  display: 'flex',
  gap: 8,
  alignItems: 'center',
  padding: '4px 6px',
  fontSize: '0.85rem',
  color: 'var(--color-text-secondary)',
  background: 'transparent',
  border: 'none',
  cursor: 'pointer',
  textAlign: 'left',
  width: '100%',
  borderRadius: 4,
}
const decisionItemStyle: React.CSSProperties = {
  borderLeft: '3px solid var(--color-border-default)',
  paddingLeft: 10,
  paddingBottom: 6,
}
const thumbButtonStyle: React.CSSProperties = {
  display: 'flex',
  flexDirection: 'column',
  gap: 4,
  padding: 6,
  border: '1px solid var(--color-border-default)',
  borderRadius: 6,
  background: 'var(--color-surface-1)',
  cursor: 'pointer',
  minWidth: 0,
}
const thumbLabelStyle: React.CSSProperties = {
  fontSize: '0.7rem',
  color: 'var(--color-text-secondary)',
  overflow: 'hidden',
  textOverflow: 'ellipsis',
  whiteSpace: 'nowrap',
}
const logPreStyle: React.CSSProperties = {
  margin: 0,
  padding: '8px 10px',
  background: 'var(--color-chrome-bg-elevated)',
  color: 'var(--color-chrome-fg-muted)',
  fontSize: '0.74rem',
  lineHeight: 1.45,
  borderRadius: 6,
  whiteSpace: 'pre-wrap',
  wordBreak: 'break-word',
  maxHeight: 260,
  overflowY: 'auto',
  fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
}
const lightboxBackdrop: React.CSSProperties = {
  position: 'fixed',
  inset: 0,
  background: 'rgba(15, 23, 42, 0.82)',
  zIndex: Z.TOAST,
  display: 'flex',
  alignItems: 'center',
  justifyContent: 'center',
  cursor: 'zoom-out',
}
const _modalBackdrop: React.CSSProperties = {
  position: 'fixed',
  inset: 0,
  background: 'rgba(15, 23, 42, 0.55)',
  zIndex: Z.NESTED_DRAWER,
  display: 'flex',
  alignItems: 'center',
  justifyContent: 'center',
}
const modalCardStyle: React.CSSProperties = {
  width: 'min(480px, 92vw)',
  background: 'var(--color-surface-1)',
  color: 'var(--color-text-primary)',
  borderRadius: 10,
  padding: '20px 22px',
  boxShadow: '0 16px 48px rgba(15, 23, 42, 0.28)',
  maxHeight: '90vh',
  overflowY: 'auto',
}
const modalLabelStyle: React.CSSProperties = {
  fontSize: '0.78rem',
  color: 'var(--color-text-secondary)',
  marginBottom: 5,
  fontWeight: 500,
}
const modalInputStyle: React.CSSProperties = {
  width: '100%',
  padding: '0.5rem 0.7rem',
  borderRadius: 5,
  border: '1px solid var(--color-border-strong)',
  background: 'var(--color-surface-1)',
  color: 'var(--color-text-primary)',
  fontSize: '0.85rem',
  fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
  boxSizing: 'border-box',
}
const previewBoxStyle: React.CSSProperties = {
  background: 'var(--color-surface-0)',
  border: '1px solid var(--color-border-default)',
  borderRadius: 6,
  padding: '10px 12px',
}

function Section({ title, children }: { title: string; children: React.ReactNode }): JSX.Element {
  return (
    <section style={sectionStyle}>
      <div style={sectionTitleStyle}>{title}</div>
      {children}
    </section>
  )
}

// Renders the claim verification rollup for one completed task. The
// server populates `taskResult.verification` from a fast-path sidecar
// or via live verification. This panel surfaces the per-claim verdict
// list and offers a Re-verify button that POSTs the verify endpoint
// (which can transition the session to `Blocked { ValidationFailed }`
// when a mismatch is found).
function ClaimVerificationPanel({
  sessionId,
  taskId,
  initialReport,
}: {
  sessionId: string
  taskId: string
  initialReport: ClaimVerificationReport
}): JSX.Element {
  const [report, setReport] = useState<ClaimVerificationReport>(initialReport)
  const [verifying, setVerifying] = useState(false)
  const [err, setErr] = useState<string | null>(null)

  useEffect(() => {
    setReport(initialReport)
  }, [initialReport])

  const onReverify = async () => {
    setVerifying(true)
    setErr(null)
    try {
      const res = await verifyTask(sessionId, taskId)
      if (res.report) {
        setReport(res.report)
      } else if (res.reason) {
        setErr(res.reason)
      }
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e))
    } finally {
      setVerifying(false)
    }
  }

  const hasMismatch = report.n_mismatch > 0

  return (
    <div style={{ fontSize: 13 }}>
      <div
        style={{
          marginBottom: 6,
          display: 'flex',
          alignItems: 'center',
          gap: 8,
        }}
      >
        <span
          style={{
            display: 'inline-block',
            padding: '1px 6px',
            borderRadius: 3,
            fontSize: 11,
            fontWeight: 600,
            background: hasMismatch ? '#fef2f2' : '#dcfce7',
            color: hasMismatch ? '#991b1b' : '#166534',
          }}
        >
          {hasMismatch ? 'MISMATCH' : 'PASS'}
        </span>
        <span style={{ color: 'var(--color-text-muted, #666)' }}>
          {report.n_verified} verified · {report.n_mismatch} mismatch ·{' '}
          {report.n_unverifiable} unverifiable ({report.n_checked} checked)
        </span>
        <button
          type="button"
          onClick={() => void onReverify()}
          disabled={verifying}
          style={{ marginLeft: 'auto', fontSize: 11, padding: '2px 8px' }}
          title="Re-run claim_extractor + claim_verifier"
        >
          {verifying ? 'Verifying…' : 'Re-verify'}
        </button>
      </div>
      {err ? (
        <div style={{ color: '#991b1b', fontSize: 12, marginBottom: 6 }}>
          {err}
        </div>
      ) : null}
      {report.verdicts.length > 0 ? (
        <ul
          style={{
            margin: 0,
            paddingLeft: 18,
            fontSize: 12,
            maxHeight: 240,
            overflowY: 'auto',
          }}
        >
          {report.verdicts.map((v, i) => (
            <ClaimVerdictRow key={i} verdict={v} />
          ))}
        </ul>
      ) : (
        <div style={{ color: 'var(--color-text-muted, #666)', fontSize: 12 }}>
          No verdicts recorded for this task.
        </div>
      )}
      {report.runtime_decision_log_path ? (
        <div style={{ fontSize: 11, color: 'var(--color-text-muted, #666)', marginTop: 6 }}>
          Cross-reference:{' '}
          <code>{report.runtime_decision_log_path}</code>
        </div>
      ) : null}
    </div>
  )
}

function ClaimVerdictRow({ verdict }: { verdict: ClaimVerdict }): JSX.Element {
  const c = verdict.claim
  const s = verdict.status
  const isVerified = s.status === 'verified'
  const isMismatch = s.status === 'mismatch'
  const detail = isMismatch
    ? s.detail
    : s.status === 'unverifiable'
      ? s.reason
      : null
  const color = isVerified ? '#16a34a' : isMismatch ? '#dc2626' : 'var(--color-text-muted, #666)'
  return (
    <li style={{ marginBottom: 4 }}>
      <span style={{ color }}>{isVerified ? '✓' : isMismatch ? '⚠' : '?'}</span>{' '}
      <code>{c.entity}</code>
      {c.direction ? ` ${c.direction}` : ''}
      {c.effect_size !== undefined && c.effect_size !== null
        ? ` (effect=${c.effect_size})`
        : ''}
      {c.pvalue !== undefined && c.pvalue !== null ? ` p=${c.pvalue}` : ''}
      {' — '}
      <span style={{ color: 'var(--color-text-muted, #666)' }}>{s.status}</span>
      {detail ? (
        <>
          : <span style={{ color: '#7c2d12' }}>{detail}</span>
        </>
      ) : null}
    </li>
  )
}
