import { describe, expect, it, vi } from 'vitest'
import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import StartExecutionPanel from './StartExecutionPanel'
import type { ProgressSummary, SessionState } from '../types'

const emitted: SessionState = { kind: 'emitted' }
const intake: SessionState = { kind: 'intake' }
const progress = (
  completed: number,
  total: number,
): ProgressSummary => ({
  completed,
  ready: Math.max(total - completed, 0),
  blocked: 0,
  pending: 0,
})

describe('StartExecutionPanel', () => {
  it('renders nothing when session state is not emitted', () => {
    const { container } = render(
      <StartExecutionPanel
        sessionId="s1"
        sessionState={intake}
        executionRunning={false}
        onStart={vi.fn()}
      />,
    )
    expect(container).toBeEmptyDOMElement()
    expect(
      screen.queryByRole('button', { name: /start execution/i }),
    ).toBeNull()
  })

  it('renders nothing when emitted but execution is already running', () => {
    const { container } = render(
      <StartExecutionPanel
        sessionId="s1"
        sessionState={emitted}
        executionRunning={true}
        onStart={vi.fn()}
      />,
    )
    expect(container).toBeEmptyDOMElement()
    expect(
      screen.queryByRole('button', { name: /start execution/i }),
    ).toBeNull()
  })

  it('renders helper line + button when emitted and not running', () => {
    render(
      <StartExecutionPanel
        sessionId="s1"
        sessionState={emitted}
        executionRunning={false}
        onStart={vi.fn()}
      />,
    )
    expect(
      screen.getByText(/package emitted\. ready to start execution\./i),
    ).toBeInTheDocument()
    expect(
      screen.getByRole('button', { name: /start execution/i }),
    ).toBeEnabled()
  })

  it('renders resume copy when a branch or partial run already has completed tasks', () => {
    render(
      <StartExecutionPanel
        sessionId="s1"
        sessionState={emitted}
        executionRunning={false}
        progress={progress(7, 17)}
        onStart={vi.fn()}
      />,
    )
    expect(
      screen.getByText(/7 of 17 tasks already complete/i),
    ).toBeInTheDocument()
    expect(
      screen.getByRole('button', { name: /resume execution/i }),
    ).toBeEnabled()
  })

  it('renders nothing when all tasks are complete', () => {
    const { container } = render(
      <StartExecutionPanel
        sessionId="s1"
        sessionState={emitted}
        executionRunning={false}
        progress={progress(17, 17)}
        onStart={vi.fn()}
      />,
    )
    expect(container).toBeEmptyDOMElement()
    expect(
      screen.queryByRole('button', { name: /start execution/i }),
    ).toBeNull()
    expect(
      screen.queryByRole('button', { name: /resume execution/i }),
    ).toBeNull()
  })

  it('invokes onStart exactly once on click', async () => {
    const user = userEvent.setup()
    const onStart = vi.fn().mockResolvedValue(undefined)
    render(
      <StartExecutionPanel
        sessionId="s1"
        sessionState={emitted}
        executionRunning={false}
        onStart={onStart}
      />,
    )
    await user.click(screen.getByRole('button', { name: /start execution/i }))
    expect(onStart).toHaveBeenCalledTimes(1)
  })

  it('shows loading state while onStart is pending and disables the button', async () => {
    const user = userEvent.setup()
    // Deferred promise — onStart never resolves so the panel stays in
    // its pending state long enough to assert the loading text and
    // the disabled attribute.
    let resolveStart: () => void = () => undefined
    const onStart = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          resolveStart = resolve
        }),
    )
    render(
      <StartExecutionPanel
        sessionId="s1"
        sessionState={emitted}
        executionRunning={false}
        onStart={onStart}
      />,
    )
    await user.click(screen.getByRole('button', { name: /start execution/i }))
    const pendingBtn = await screen.findByRole('button', {
      name: /starting execution/i,
    })
    expect(pendingBtn).toBeDisabled()
    // Cleanup so the dangling promise doesn't leak into the next test.
    resolveStart()
  })

  it('surfaces an inline error and re-enables the button when onStart rejects', async () => {
    const user = userEvent.setup()
    const onStart = vi.fn().mockRejectedValue(new Error('502 Bad Gateway'))
    render(
      <StartExecutionPanel
        sessionId="s1"
        sessionState={emitted}
        executionRunning={false}
        onStart={onStart}
      />,
    )
    await user.click(screen.getByRole('button', { name: /start execution/i }))
    expect(
      await screen.findByText(/502 bad gateway/i),
    ).toBeInTheDocument()
    expect(
      screen.getByRole('button', { name: /start execution/i }),
    ).toBeEnabled()
  })
})
