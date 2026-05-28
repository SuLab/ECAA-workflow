import { describe, expect, it, vi } from 'vitest'
import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import BranchFromHereCard from './BranchFromHereCard'

describe('BranchFromHereCard', () => {
  it('renders the title and a branch button by default', () => {
    render(<BranchFromHereCard onBranch={vi.fn()} />)
    expect(
      screen.getByText(/Want to try an alternative without losing this analysis/i),
    ).toBeInTheDocument()
    expect(
      screen.getByRole('button', { name: /Branch from here/i }),
    ).toBeInTheDocument()
  })

  it('clicking Branch expands the rationale panel', async () => {
    const user = userEvent.setup()
    render(<BranchFromHereCard onBranch={vi.fn()} />)
    await user.click(screen.getByRole('button', { name: /Branch from here/i }))
    expect(
      screen.getByLabelText(/Optional branching rationale/i),
    ).toBeInTheDocument()
    expect(
      screen.getByRole('button', { name: /Create branch/i }),
    ).toBeInTheDocument()
  })

  it('Create branch dispatches onBranch with the typed rationale', async () => {
    const user = userEvent.setup()
    const onBranch = vi.fn().mockResolvedValue(undefined)
    render(<BranchFromHereCard onBranch={onBranch} />)
    await user.click(screen.getByRole('button', { name: /Branch from here/i }))
    await user.type(
      screen.getByLabelText(/Optional branching rationale/i),
      'try scVI integration',
    )
    await user.click(screen.getByRole('button', { name: /Create branch/i }))
    expect(onBranch).toHaveBeenCalledWith('try scVI integration')
  })

  it('passes undefined rationale when textarea is blank', async () => {
    const user = userEvent.setup()
    const onBranch = vi.fn().mockResolvedValue(undefined)
    render(<BranchFromHereCard onBranch={onBranch} />)
    await user.click(screen.getByRole('button', { name: /Branch from here/i }))
    await user.click(screen.getByRole('button', { name: /Create branch/i }))
    expect(onBranch).toHaveBeenCalledWith(undefined)
  })

  it('Cancel closes the panel without dispatching', async () => {
    const user = userEvent.setup()
    const onBranch = vi.fn()
    render(<BranchFromHereCard onBranch={onBranch} />)
    await user.click(screen.getByRole('button', { name: /Branch from here/i }))
    await user.click(screen.getByRole('button', { name: /^Cancel$/i }))
    expect(onBranch).not.toHaveBeenCalled()
    expect(
      screen.queryByLabelText(/Optional branching rationale/i),
    ).not.toBeInTheDocument()
  })

  it('disabled prop greys the branch button and prevents expansion', async () => {
    const user = userEvent.setup()
    render(<BranchFromHereCard onBranch={vi.fn()} disabled />)
    const button = screen.getByRole('button', { name: /Branch from here/i })
    expect(button).toBeDisabled()
    await user.click(button) // click should be a no-op since disabled
    expect(
      screen.queryByLabelText(/Optional branching rationale/i),
    ).not.toBeInTheDocument()
  })
})
