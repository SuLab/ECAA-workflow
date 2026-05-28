// Unit tests for the event branches in `useSseChatEvents`. The hook
// opens one EventSource per session via `connectChatStream`; tests
// mock that module so we can inject synthetic events through the
// onEvent callback captured on connect.

import { act, renderHook } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { ChatSseEvent } from '../api/chatStream'

// Capture the onEvent callback `connectChatStream` is handed by the
// hook so tests can fire synthetic events through it. `vi.hoisted`
// ensures the captured reference is set up *before* the module is
// mocked.
const captured = vi.hoisted(() => ({
  onEvent: null as ((e: ChatSseEvent) => void) | null,
  /// Tracks connect / disconnect counts so tests can assert that
  /// re-renders with new opts identities do NOT re-create the EventSource.
  connectCount: 0,
  disconnectCount: 0,
}))

vi.mock('../api/chatStream', () => ({
  connectChatStream: (_sessionId: string, onEvent: (e: ChatSseEvent) => void) => {
    captured.onEvent = onEvent
    captured.connectCount++
    return () => {
      captured.onEvent = null
      captured.disconnectCount++
    }
  },
}))

import { useSseChatEvents } from './useSseChatEvents'

function fireEvent(e: ChatSseEvent) {
  if (!captured.onEvent) {
    throw new Error(
      'connectChatStream mock was not captured — hook did not subscribe',
    )
  }
  act(() => captured.onEvent!(e))
}

describe('useSseChatEvents — event handlers', () => {
  beforeEach(() => {
    captured.onEvent = null
    captured.connectCount = 0
    captured.disconnectCount = 0
  })

  it('sets pilot.status to "started" when harness_sizing_pilot_started arrives', () => {
    const { result } = renderHook(() => useSseChatEvents('sess-1'))
    expect(result.current.pilot.status).toBeNull()
    fireEvent({ type: 'harness_sizing_pilot_started' })
    expect(result.current.pilot.status).toBe('started')
    expect(result.current.pilot.report).toBeNull()
  })

  it('sets pilot.report to the event payload when harness_sizing_pilot_complete arrives', () => {
    const { result } = renderHook(() => useSseChatEvents('sess-2'))
    const reportPayload = {
      measurements: [{ task_id: 't1', peak_rss_mb: 500 }],
      confidence: 0.82,
    }
    fireEvent({ type: 'harness_sizing_pilot_complete', report: reportPayload })
    expect(result.current.pilot.status).toBe('complete')
    expect(result.current.pilot.report).toEqual(reportPayload)
  })

  it('populates stallSignals[task_id] when harness_stall_detected arrives', () => {
    const { result } = renderHook(() => useSseChatEvents('sess-3'))
    expect(result.current.stallSignals).toEqual({})
    fireEvent({
      type: 'harness_stall_detected',
      task_id: 'align-1',
      signal: { kind: 'memory_pressure', pct: 93, window_mins: 5 },
      suggested_action: 'resize',
    })
    expect(result.current.stallSignals['align-1']).toEqual({
      taskId: 'align-1',
      signal: { kind: 'memory_pressure', pct: 93, window_mins: 5 },
      suggestedAction: 'resize',
    })
  })

  it('latest stall signal wins when the same task_id reports twice', () => {
    const { result } = renderHook(() => useSseChatEvents('sess-4'))
    fireEvent({
      type: 'harness_stall_detected',
      task_id: 'align-1',
      signal: { kind: 'cpu_starvation', avg_cpu_pct: 1.0, window_mins: 30 },
      suggested_action: 'retry',
    })
    fireEvent({
      type: 'harness_stall_detected',
      task_id: 'align-1',
      signal: { kind: 'memory_pressure', pct: 94, window_mins: 5 },
      suggested_action: 'resize',
    })
    expect(result.current.stallSignals['align-1']?.suggestedAction).toBe('resize')
  })

  /// Every declared `ChatSseEvent` variant is dispatched through the
  /// registry without a runtime error. The exhaustiveness check is
  /// enforced by `tsc` (the registry type spans every variant); this
  /// runtime test is the compile-error's belt-and-suspenders mate.
  it('dispatches every known ChatSseEvent variant without error', () => {
    renderHook(() => useSseChatEvents('sess-registry'))
    const events: ChatSseEvent[] = [
      { type: 'tool_call_started', tool_name: 't', status_line: 's' },
      { type: 'tool_call_finished', tool_name: 't' },
      { type: 'assistant_token_delta', text: 'x' },
      { type: 'state_advanced', new_state: { kind: 'intake' } },
      {
        type: 'harness_progress',
        kind: 'task_started',
        task_id: 't1',
        status: 'ok',
        detail: '',
      },
      { type: 'infra_error', reason: 'x', user_copy: 'y' },
      {
        type: 'package_amended',
        session_id: 's',
        amended_stage: 'z',
        invalidated_tasks: [],
        package_path: '/p',
      },
      { type: 'task_completed_reviewable', task_id: 't1', artifacts: [] },
      { type: 'harness_sizing_pilot_started' },
      { type: 'harness_sizing_pilot_complete', report: {} },
      { type: 'harness_sizing_pilot_skipped', reason: 'x' },
      {
        type: 'harness_stall_detected',
        task_id: 't1',
        signal: { kind: 'cpu_starvation', avg_cpu_pct: 1, window_mins: 10 },
        suggested_action: 'retry',
      },
      {
        type: 'harness_resize_recommended',
        task_id: 't1',
        from_instance_type: 'r6i.xlarge',
        to_instance_type: 'r6i.2xlarge',
      },
      { type: 'harness_version_diff', report: {} },
      { type: 'resync_required', dropped: 0 },
      { type: 'dashboard_summary_failed', task_id: 't1', reason: 'boom' },
      {
        type: 'turn_appended',
        turn: {
          turn_id: 'turn-1',
          role: 'assistant',
          content: 'done',
          intent: null,
          tool_calls: [],
          quick_replies: [],
          confirmation_card: null,
          timestamp: '2026-05-14T00:00:00Z',
        },
      },
      {
        type: 'harness_executor_selected',
        name: 'aws',
        cpu_budget: 8,
        gpu_budget: 1,
        instance_type: 'r6i.xlarge',
        harness_version: '0.1.0',
        env_mode: 'test',
      },
      {
        type: 'harness_progress_health',
        total_posts: 2,
        failed_posts: 0,
        total_attempts: 2,
        last_error: '',
        last_success_at: '2026-05-14T00:00:00Z',
      },
      {
        type: 'harness_orphans_reaped',
        candidate_count: 1,
        verified_count: 1,
        unverified_ids: [],
        policy: 'warn',
      },
      { type: 'harness_heartbeat_stalled', task_id: 't1', age_secs: 90 },
      { type: 'proposal_received', proposal_id: 'p1', node_id: 'n1' },
      {
        type: 'proposal_gate_advanced',
        proposal_id: 'p1',
        gate: 'validator',
        passed: true,
      },
      { type: 'proposal_promoted', proposal_id: 'p1', task_node_id: 'task-n1' },
      { type: 'proposal_rejected', proposal_id: 'p2', rationale: null },
    ]
    for (const e of events) {
      // Must not throw — registry must have a handler for every variant.
      expect(() => fireEvent(e)).not.toThrow()
    }
  })

  it('normalizes remote metadata on live harness_progress events', () => {
    const { result } = renderHook(() => useSseChatEvents('sess-remote'))
    fireEvent({
      type: 'harness_progress',
      kind: 'task_started',
      task_id: 'align-remote',
      status: 'running',
      detail: 'started on aws',
      remote: {
        backend: 'aws',
        instance_id: 'i-123',
        instance_type: 'r6i.xlarge',
      },
    })
    expect(result.current.harnessProgress[0]).toMatchObject({
      kind: 'task_started',
      taskId: 'align-remote',
      remote: {
        backend: 'aws',
        instanceId: 'i-123',
        instanceType: 'r6i.xlarge',
      },
    })
  })

  it('surfaces dashboard_summary_failed as an infra error', () => {
    const { result } = renderHook(() => useSseChatEvents('sess-dashboard-fail'))
    fireEvent({
      type: 'dashboard_summary_failed',
      task_id: 'summary-task',
      reason: 'sidecar timeout',
    })
    expect(result.current.infraError).toEqual({
      reason: 'dashboard_summary_failed',
      userCopy:
        'Dashboard summary failed for task summary-task: sidecar timeout',
    })
  })

  it('warns instead of throwing for an unknown SSE event type', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    renderHook(() => useSseChatEvents('sess-unknown'))
    expect(() =>
      fireEvent({ type: 'future_event' } as unknown as ChatSseEvent),
    ).not.toThrow()
    expect(warn).toHaveBeenCalledWith(
      '[useSseChatEvents] no handler for SSE type',
      'future_event',
      { type: 'future_event' },
    )
    warn.mockRestore()
  })

  it('caps harnessProgress at 500 with drop-oldest and tracks dropped count', () => {
    const { result } = renderHook(() => useSseChatEvents('sess-cap'))
    // Push 600 events; buffer should hold only the newest 500 and
    // report 100 drops.
    for (let i = 0; i < 600; i++) {
      fireEvent({
        type: 'harness_progress',
        kind: 'task_started',
        task_id: `t${i}`,
        status: 'running',
        detail: '',
      })
    }
    expect(result.current.harnessProgress.length).toBe(500)
    expect(result.current.harnessProgress[0]!.taskId).toBe('t100')
    expect(result.current.harnessProgress[499]!.taskId).toBe('t599')
    expect(result.current.harnessProgressDropped).toBe(100)
  })

  it('fires onResyncRequired and bumps dropped counter on resync_required', async () => {
    const onResync = vi.fn()
    const { result } = renderHook(() =>
      useSseChatEvents('sess-resync', { onResyncRequired: onResync }),
    )
    fireEvent({ type: 'resync_required', dropped: 42 })
    // Synchronous callback invocation path; the hook fires-and-forgets.
    await vi.waitFor(() => expect(onResync).toHaveBeenCalledWith(42))
    expect(result.current.harnessProgressDropped).toBe(42)
  })

  it('sets crossVersionReport when harness_version_diff arrives', () => {
    const { result } = renderHook(() => useSseChatEvents('sess-5'))
    expect(result.current.crossVersionReport).toBeNull()
    const diff = {
      parent_package: '/pkgs/parent',
      child_package: '/pkgs/child',
      overall_concordance: 0.87,
      tables: [
        { table_name: 'deg', n_robust: 120, n_concordant: 18, n_discordant: 5 },
      ],
    }
    fireEvent({ type: 'harness_version_diff', report: diff })
    expect(result.current.crossVersionReport).toEqual(diff)
  })

  /// Re-rendering with a new `opts` object identity must NOT tear down
  /// and re-open the EventSource. The handlers read `opts` through a
  /// ref so dispatch sees live callbacks while the subscription stays
  /// stable across renders. A `[sessionId]` dep array combined with
  /// handlers that closed over the original render's `opts` would
  /// silently fire stale callbacks.
  it('does not re-subscribe when opts identity changes', () => {
    const onStateAdvanced1 = vi.fn()
    const onStateAdvanced2 = vi.fn()
    const { rerender } = renderHook(
      ({ cb }) => useSseChatEvents('sess-opts', { onStateAdvanced: cb }),
      { initialProps: { cb: onStateAdvanced1 } },
    )
    expect(captured.connectCount).toBe(1)
    expect(captured.disconnectCount).toBe(0)
    // Re-render with a fresh callback identity (same sessionId).
    rerender({ cb: onStateAdvanced2 })
    expect(captured.connectCount).toBe(1)
    expect(captured.disconnectCount).toBe(0)
  })

  /// After the parent re-renders with a new `onStateAdvanced`, an SSE
  /// event must invoke the LATEST callback, not the first-render stale one.
  it('invokes the latest opts callback after a re-render', () => {
    const stale = vi.fn()
    const fresh = vi.fn()
    const { rerender } = renderHook(
      ({ cb }) => useSseChatEvents('sess-fresh', { onStateAdvanced: cb }),
      { initialProps: { cb: stale } },
    )
    rerender({ cb: fresh })
    fireEvent({ type: 'state_advanced', new_state: { kind: 'intake' } })
    expect(stale).not.toHaveBeenCalled()
    expect(fresh).toHaveBeenCalledTimes(1)
  })

  /// Forwards the SSE payload's `new_state` to the `onStateAdvanced`
  /// callback so the consumer can apply it synchronously instead of
  /// triggering an unguarded refetch. Without this, two consecutive
  /// `task_blocked` events for different tasks (the common discover_*
  /// flow) could race the refetch and the second BlockerCard would
  /// fail to render — the production bug this hook closes.
  it('forwards new_state payload to onStateAdvanced', () => {
    const onStateAdvanced = vi.fn()
    renderHook(() =>
      useSseChatEvents('sess-payload', { onStateAdvanced }),
    )
    const newState = {
      kind: 'blocked' as const,
      reason: 'Task discover_normalisation blocked: Awaiting SME approval',
      recovery_hint: 'Review the candidates.',
      blockers: [
        {
          blocker_id: 'bid-1',
          task_id: 'discover_normalisation',
          kind: {
            kind: 'awaiting_sme_approval' as const,
            stage_id: 'discover_normalisation',
            top_candidate: 'deseq2_vst',
            runner_ups: [],
          },
          message:
            'Task discover_normalisation blocked: Awaiting SME approval',
          at: '2026-05-24T17:00:00Z',
        },
      ],
    }
    fireEvent({ type: 'state_advanced', new_state: newState })
    expect(onStateAdvanced).toHaveBeenCalledTimes(1)
    expect(onStateAdvanced).toHaveBeenCalledWith(newState)
  })
})
