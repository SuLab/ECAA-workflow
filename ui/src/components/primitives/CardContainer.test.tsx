// Primitive component coverage. CardContainer is the
// shared chrome under every TurnCard variant; broken styling propagates
// through the entire chat surface, so an explicit smoke test on the
// palette + role + aria-label + data-attrs contract guards against
// silent regressions when styles/palettes.ts is touched.

import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { CardContainer } from './CardContainer'

describe('CardContainer', () => {
  it('renders the title and children', () => {
    render(
      <CardContainer palette="info" title="Plan summary" role="region">
        <p data-testid="body">Body content</p>
      </CardContainer>
    )
    expect(screen.getByText('Plan summary')).toBeInTheDocument()
    expect(screen.getByTestId('body')).toBeInTheDocument()
  })

  it('uses the title as the aria-label fallback', () => {
    render(
      <CardContainer palette="branch" title="Branch from here" role="region">
        <p>Body</p>
      </CardContainer>
    )
    const section = screen.getByRole('region', { name: 'Branch from here' })
    expect(section).toBeInTheDocument()
  })

  it('honours the explicit ariaLabel over the title', () => {
    render(
      <CardContainer
        palette="success"
        title="Visible header"
        ariaLabel="Different aria"
        role="region"
      >
        <p>Body</p>
      </CardContainer>
    )
    const section = screen.getByRole('region', { name: 'Different aria' })
    expect(section).toBeInTheDocument()
  })

  it('renders without a title when omitted', () => {
    render(
      <CardContainer palette="info">
        <p data-testid="body">Body</p>
      </CardContainer>
    )
    expect(screen.getByTestId('body')).toBeInTheDocument()
    expect(screen.queryByRole('heading')).toBeNull()
  })

  it('emits role="alert" when explicitly requested', () => {
    render(
      <CardContainer palette="danger" role="alert" ariaLabel="Blocker">
        <p>Blocker body</p>
      </CardContainer>
    )
    const alert = screen.getByRole('alert', { name: 'Blocker' })
    expect(alert).toBeInTheDocument()
  })

  it('threads data attributes onto the underlying section', () => {
    render(
      <CardContainer
        palette="danger"
        title="Stalled"
        role="region"
        dataAttrs={{ 'data-blocker-kind': 'heartbeat_stalled' }}
      >
        <p>Body</p>
      </CardContainer>
    )
    const section = screen.getByRole('region', { name: 'Stalled' })
    expect(section).toHaveAttribute('data-blocker-kind', 'heartbeat_stalled')
  })
})
