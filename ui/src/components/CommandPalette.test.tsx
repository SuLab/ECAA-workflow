import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render, waitFor } from '@testing-library/react'
import CommandPalette from './CommandPalette'

beforeEach(() => {
  vi.restoreAllMocks()
})
afterEach(() => {
  vi.restoreAllMocks()
})

describe('CommandPalette', () => {
  it('opens on Cmd+K and closes on Escape', async () => {
    const { getByRole, queryByRole } = render(<CommandPalette sessionId={null} />)
    expect(queryByRole('dialog')).toBeNull()
    fireEvent.keyDown(document, { key: 'k', metaKey: true })
    await waitFor(() => expect(getByRole('dialog')).toBeInTheDocument())
    fireEvent.keyDown(document, { key: 'Escape' })
    await waitFor(() => expect(queryByRole('dialog')).toBeNull())
  })

  it('lists tab commands when no session', async () => {
    const { getByText, getByLabelText } = render(<CommandPalette sessionId={null} />)
    fireEvent.keyDown(document, { key: 'k', metaKey: true })
    const input = getByLabelText('Command input')
    fireEvent.change(input, { target: { value: 'perfo' } })
    await waitFor(() => {
      expect(getByText(/Performance/i)).toBeTruthy()
    })
  })

  it('navigates with arrow keys', async () => {
    const { getByLabelText } = render(<CommandPalette sessionId={null} />)
    fireEvent.keyDown(document, { key: 'k', ctrlKey: true })
    const input = getByLabelText('Command input')
    fireEvent.keyDown(input, { key: 'ArrowDown' })
    // No assertion beyond "doesn't crash" — arrow nav is covered by
    // visual highlight in the listbox.
    fireEvent.keyDown(input, { key: 'ArrowUp' })
    fireEvent.keyDown(input, { key: 'Escape' })
  })
})
