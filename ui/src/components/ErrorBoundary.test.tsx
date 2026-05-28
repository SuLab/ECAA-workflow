// Coverage: catches a render exception, shows the fallback alert with
// the configured label, and recovers via the "Try again" button.

import { fireEvent, render } from '@testing-library/react'
import { useState } from 'react'
import { describe, expect, it, vi } from 'vitest'

import ErrorBoundary from './ErrorBoundary'

function Bomb({ throwError }: { throwError: boolean }): JSX.Element {
  if (throwError) throw new Error('boom from inner')
  return <div>inner ok</div>
}

describe('ErrorBoundary', () => {
  it('renders children when nothing throws', () => {
    const { getByText } = render(
      <ErrorBoundary fallbackLabel="the chat">
        <Bomb throwError={false} />
      </ErrorBoundary>,
    )
    expect(getByText('inner ok')).toBeInTheDocument()
  })

  it('renders the fallback alert with the configured label when a child throws', () => {
    // React logs the caught error to console.error; silence it for this
    // assertion so the test output stays clean.
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {})
    const { getByRole, getByText } = render(
      <ErrorBoundary fallbackLabel="the chat">
        <Bomb throwError />
      </ErrorBoundary>,
    )
    const alert = getByRole('alert')
    expect(alert).toHaveTextContent(/something went wrong rendering the chat/i)
    expect(getByText('boom from inner')).toBeInTheDocument()
    consoleError.mockRestore()
  })

  it('Try again resets the boundary and re-renders the (now-passing) child', () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {})
    // Use real React state to flip the bomb so the boundary's reset →
    // child re-render sees the new prop on the next React tick.
    const Harness = () => {
      const [throwError, setThrowError] = useState(true)
      return (
        <>
          <button data-testid="defuse" onClick={() => setThrowError(false)}>
            defuse
          </button>
          <ErrorBoundary fallbackLabel="x">
            <Bomb throwError={throwError} />
          </ErrorBoundary>
        </>
      )
    }
    const { getByRole, getByTestId, getByText } = render(<Harness />)
    expect(getByRole('alert')).toBeInTheDocument()

    // Defuse the bomb first so the next subtree mount succeeds, then
    // reset the boundary.
    fireEvent.click(getByTestId('defuse'))
    fireEvent.click(getByRole('button', { name: /try again/i }))

    expect(getByText('inner ok')).toBeInTheDocument()
    consoleError.mockRestore()
  })
})
