// mutations. Tests mount the toast inside UndoStackProvider and push
// tokens via a small TestHarness button so the provider drives the
// state transitions the same way the real callers do.

import { afterEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import UndoToast from './UndoToast'
import { UndoStackProvider, useUndoStack } from '../hooks/useUndoStack'

function TestHarness({ label, undo }: { label: string; undo: () => Promise<void> }) {
  const { push } = useUndoStack()
  return (
    <button
      type="button"
      onClick={() =>
        push({
          kind: 'amend',
          label,
          undo,
        })
      }
    >
      push token
    </button>
  )
}

describe('UndoToast', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('renders nothing when stack is empty', () => {
    const { container } = render(
      <UndoStackProvider>
        <UndoToast />
      </UndoStackProvider>,
    )
    expect(container.firstChild).toBeNull()
  })

  it('renders the token label after a push + role="status" + aria-live="polite"', async () => {
    render(
      <UndoStackProvider>
        <TestHarness label="Amended normalization" undo={vi.fn().mockResolvedValue(undefined)} />
        <UndoToast />
      </UndoStackProvider>,
    )
    fireEvent.click(screen.getByText('push token'))
    await waitFor(() => expect(screen.getByRole('status')).toBeInTheDocument())
    expect(screen.getByRole('status')).toHaveAttribute('aria-live', 'polite')
    expect(screen.getByText('Amended normalization')).toBeInTheDocument()
  })

  it("clicking Undo fires the token's undo() callback and clears the toast", async () => {
    const undo = vi.fn().mockResolvedValue(undefined)
    render(
      <UndoStackProvider>
        <TestHarness label="Reverted rerun" undo={undo} />
        <UndoToast />
      </UndoStackProvider>,
    )
    fireEvent.click(screen.getByText('push token'))
    await waitFor(() => expect(screen.getByRole('status')).toBeInTheDocument())
    fireEvent.click(screen.getByRole('button', { name: 'Undo' }))
    await waitFor(() => expect(undo).toHaveBeenCalledTimes(1))
    await waitFor(() => expect(screen.queryByRole('status')).toBeNull())
  })

  it('Escape key clears the toast', async () => {
    render(
      <UndoStackProvider>
        <TestHarness label="x" undo={vi.fn().mockResolvedValue(undefined)} />
        <UndoToast />
      </UndoStackProvider>,
    )
    fireEvent.click(screen.getByText('push token'))
    await waitFor(() => expect(screen.getByRole('status')).toBeInTheDocument())
    fireEvent.keyDown(document, { key: 'Escape' })
    await waitFor(() => expect(screen.queryByRole('status')).toBeNull())
  })
})
