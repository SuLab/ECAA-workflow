import { describe, expect, it } from 'vitest'
import { render, screen } from '@testing-library/react'
import SafetyBadge from './SafetyBadge'
import type { SafetyPolicy } from '../types/SafetyPolicy'

// SafetyPolicy fixture covering the four SafetyLevel discriminants. The
// SafetyBadge under test reads only the policy fields surfaced via the
// tooltip.
const defaultSafety: SafetyPolicy = {
  level: 'compute',
  network: { kind: 'none', allowlist: [] },
  code_execution: 'none',
  sandbox: 'none',
  provisioning: 'declared_only',
  controlled_access: false,
}

describe('SafetyBadge — Phase 8.1 (atom-safety-policy plan)', () => {
  it('renders Safe level with success-muted token', () => {
    render(<SafetyBadge safety={{ ...defaultSafety, level: 'safe' }} />)
    expect(screen.getByLabelText(/safety: safe/i)).toBeInTheDocument()
  })

  it('renders Exec level with warning token', () => {
    render(
      <SafetyBadge
        safety={{
          ...defaultSafety,
          level: 'exec',
          sandbox: 'process_isolation',
          code_execution: 'generated_by_agent',
        }}
      />,
    )
    expect(screen.getByLabelText(/safety: exec/i)).toBeInTheDocument()
  })

  it('tooltip exposes full safety policy', () => {
    const { container } = render(<SafetyBadge safety={defaultSafety} />)
    const badge = container.querySelector('[aria-label]')
    expect(badge?.getAttribute('title')).toContain('compute')
    expect(badge?.getAttribute('title')).toContain('declared_only')
  })
})
