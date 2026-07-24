import { afterEach, describe, expect, it, vi } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'

// The drawer reads the App-owned conversation via context; a minimal stand-in
// keeps the render focused on task-shape resilience.
const h = vi.hoisted(() => ({
  session: {
    state: { parent_session_id: null, state: { kind: 'emitted' } },
    switchToSession: vi.fn(),
  } as Record<string, unknown>,
}))
vi.mock('../hooks/contexts', () => ({
  useSessionContext: () => h.session,
}))

import TaskDetailDrawer from './TaskDetailDrawer'
import { UndoStackProvider } from '../hooks/useUndoStack'
import type { DAG } from '../types/DAG'

// A reconstructed (imported / read-only) package's `/dag` endpoint serves tasks
// WITHOUT `inputs`/`outputs`/`spec`/`edam_operation`: the DAG → WorkflowDag →
// DAG round-trip on import drops per-task port metadata. The `Task` ts-rs type
// still declares `inputs`/`outputs` as non-optional, so the drawer trusted them
// and called `task.inputs.filter(...)` — crashing the whole state inspector
// ("Cannot read properties of undefined (reading 'filter')") the moment an SME
// clicked any node in an imported package. Regression guard: the drawer must
// open for such a task rather than white-screening.
function importedTaskDag(): DAG {
  return {
    workflow_id: 'wf',
    version: '0.1',
    schema_version: '0.1',
    current_task: null,
    execution_order: ['differential_expression'],
    tasks: {
      differential_expression: {
        description: 'Test for differentially expressed genes',
        kind: 'compute',
        execution_index: 10,
        depends_on: [],
        assignee: 'agent',
        resource_class: 'standard',
        state: { status: 'completed' },
        // NB: no inputs / outputs / spec / edam_operation — exactly the shape
        // the imported /dag endpoint serves.
      },
    },
  } as unknown as DAG
}

describe('TaskDetailDrawer — imported task missing inputs/outputs', () => {
  afterEach(() => vi.restoreAllMocks())

  it('opens without crashing when task.inputs/outputs are undefined', async () => {
    // Every drawer fetch 404s → the drawer falls back to empty states.
    vi.stubGlobal(
      'fetch',
      vi.fn(() => Promise.resolve(new Response('not found', { status: 404 }))),
    )

    render(
      <UndoStackProvider>
        <TaskDetailDrawer
          sessionId="s1"
          taskId="differential_expression"
          dag={importedTaskDag()}
          onClose={() => {}}
        />
      </UndoStackProvider>,
    )

    // The drawer mounted (not the ErrorBoundary fallback): its container and the
    // task-id header are present.
    await waitFor(() =>
      expect(screen.getByTestId('task-detail-drawer')).toBeInTheDocument(),
    )
    expect(screen.getByText('differential_expression')).toBeInTheDocument()
  })
})
