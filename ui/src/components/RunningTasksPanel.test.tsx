import { afterEach, describe, expect, it, vi } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import RunningTasksPanel from './RunningTasksPanel'
import * as chatClient from '../api/chatClient'

describe('RunningTasksPanel', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('renders nothing when no sessionId is set', () => {
    const { container } = render(<RunningTasksPanel sessionId={null} />)
    expect(container.firstChild).toBeNull()
  })

  it('renders nothing when no tasks are running', async () => {
    vi.spyOn(chatClient, 'getActiveTasks').mockResolvedValue([])
    const { container } = render(<RunningTasksPanel sessionId="s-1" />)
    await waitFor(() => {
      expect(chatClient.getActiveTasks).toHaveBeenCalledWith('s-1')
    })
    // After the resolved fetch, panel still has no content.
    expect(container.querySelector('[data-testid="running-tasks-panel"]')).toBeNull()
  })

  it('renders one card per running task with friendly name + heartbeat', async () => {
    const ago = (s: number) =>
      new Date(Date.now() - s * 1_000).toISOString()
    vi.spyOn(chatClient, 'getActiveTasks').mockResolvedValue([
      {
        task_id: 'data_acquisition',
        stage_class: 'data_acquisition',
        friendly_name: 'Data Acquisition',
        started_at: ago(125), // 2m 5s elapsed
        elapsed_secs: 125,
        heartbeat_age_secs: 30, // green
        last_progress_line: 'fetching GSE189916',
        progress: { kind: 'determinate', completed: 33, total: 70, unit: 'libs' },
      },
      {
        task_id: 'compute_qc',
        stage_class: 'quality_control',
        friendly_name: 'Quality Control',
        started_at: ago(45),
        elapsed_secs: 45,
        heartbeat_age_secs: null, // starting
        last_progress_line: null,
        progress: {
          kind: 'indeterminate',
          eta_min_secs: null,
          eta_max_secs: null,
        },
      },
    ])
    render(<RunningTasksPanel sessionId="s-1" />)
    await waitFor(() => {
      expect(screen.getByTestId('running-tasks-panel')).toBeInTheDocument()
    })
    const cards = screen.getAllByTestId('active-task-card')
    expect(cards).toHaveLength(2)
    expect(screen.getByText('Data Acquisition')).toBeInTheDocument()
    expect(screen.getByText('Quality Control')).toBeInTheDocument()
    expect(screen.getByText('fetching GSE189916')).toBeInTheDocument()
    // Determinate progressbar with concrete values
    const determinate = screen.getByLabelText('33 of 70 libs')
    expect(determinate).toHaveAttribute('aria-valuenow', '33')
    expect(determinate).toHaveAttribute('aria-valuemax', '70')
    // Indeterminate variant
    const indeterminate = screen.getByLabelText('Task in progress (indeterminate)')
    expect(indeterminate).toHaveAttribute('data-indeterminate', 'true')
  })

  it('removes a card when the task transitions out of running', async () => {
    const ago = (s: number) =>
      new Date(Date.now() - s * 1_000).toISOString()
    const spy = vi.spyOn(chatClient, 'getActiveTasks')
    spy.mockResolvedValueOnce([
      {
        task_id: 't1',
        stage_class: 'data_acquisition',
        friendly_name: 'Data Acquisition',
        started_at: ago(10),
        elapsed_secs: 10,
        heartbeat_age_secs: 5,
        last_progress_line: null,
        progress: {
          kind: 'indeterminate',
          eta_min_secs: null,
          eta_max_secs: null,
        },
      },
    ])
    render(<RunningTasksPanel sessionId="s-1" />)
    await waitFor(() => {
      expect(screen.getByText('Data Acquisition')).toBeInTheDocument()
    })
    // Subsequent poll: empty list → card disappears. The component's
    // setInterval(load, 2000) fires the next fetch; with real timers
    // we just wait for it.
    spy.mockResolvedValueOnce([])
    await waitFor(
      () => {
        expect(screen.queryByText('Data Acquisition')).not.toBeInTheDocument()
      },
      { timeout: 4_000 },
    )
  })
})
