/**
 * Unit tests for AgentCodeSubtab.
 *
 * Covers:
 *  1. Renders placeholder when agentCode is null.
 *  2. Renders placeholder when agentCode is undefined.
 *  3. Renders executed code block when present.
 *  4. Does not render executed code block when it is empty.
 *  5. Renders response text block when present.
 *  6. Does not render response text block when it is empty.
 *  7. Renders prompt block when present.
 *  8. Language badge reflects the `language` field.
 *  9. Timestamps are rendered as <time> elements with correct dateTime.
 */
import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import AgentCodeSubtab from '../AgentCodeSubtab'
import type { AgentCodeRecord } from '../../api/chatClient'

const FULL_RECORD: AgentCodeRecord = {
  prompt: 'Run the alignment with STAR',
  response_text: '',
  executed_code: '#!/usr/bin/env bash\nSTAR --runMode alignReads',
  language: 'Bash',
  started_at: '2026-05-22T10:00:00Z',
  completed_at: '2026-05-22T10:05:30Z',
}

const PYTHON_RECORD: AgentCodeRecord = {
  prompt: 'Normalize counts',
  response_text: 'I will normalize using DESeq2',
  executed_code: 'import pandas as pd\ncounts.normalize()',
  language: 'Python',
  started_at: '2026-05-22T09:00:00Z',
  completed_at: '2026-05-22T09:10:00Z',
}

describe('AgentCodeSubtab', () => {
  it('renders placeholder when agentCode is null', () => {
    render(<AgentCodeSubtab agentCode={null} />)
    expect(screen.getByTestId('agent-code-absent')).toBeInTheDocument()
    expect(screen.queryByTestId('agent-code-subtab')).not.toBeInTheDocument()
  })

  it('renders placeholder when agentCode is undefined', () => {
    render(<AgentCodeSubtab agentCode={undefined} />)
    expect(screen.getByTestId('agent-code-absent')).toBeInTheDocument()
  })

  it('renders the subtab container when agentCode is present', () => {
    render(<AgentCodeSubtab agentCode={FULL_RECORD} />)
    expect(screen.getByTestId('agent-code-subtab')).toBeInTheDocument()
    expect(screen.queryByTestId('agent-code-absent')).not.toBeInTheDocument()
  })

  it('renders executed code when non-empty', () => {
    render(<AgentCodeSubtab agentCode={FULL_RECORD} />)
    const block = screen.getByTestId('agent-code-executed')
    expect(block).toBeInTheDocument()
    expect(block.textContent).toContain('STAR --runMode alignReads')
  })

  it('does not render executed code block when it is empty', () => {
    const rec: AgentCodeRecord = { ...FULL_RECORD, executed_code: '' }
    render(<AgentCodeSubtab agentCode={rec} />)
    expect(screen.queryByTestId('agent-code-executed')).not.toBeInTheDocument()
  })

  it('renders response text block when non-empty', () => {
    render(<AgentCodeSubtab agentCode={PYTHON_RECORD} />)
    const block = screen.getByTestId('agent-code-response')
    expect(block).toBeInTheDocument()
    expect(block.textContent).toContain('normalize using DESeq2')
  })

  it('does not render response text block when empty', () => {
    render(<AgentCodeSubtab agentCode={FULL_RECORD} />)
    // FULL_RECORD has empty response_text
    expect(screen.queryByTestId('agent-code-response')).not.toBeInTheDocument()
  })

  it('renders prompt block when present', () => {
    render(<AgentCodeSubtab agentCode={FULL_RECORD} />)
    const block = screen.getByTestId('agent-code-prompt')
    expect(block).toBeInTheDocument()
    expect(block.textContent).toContain('Run the alignment with STAR')
  })

  it('reflects the language field in the badge', () => {
    render(<AgentCodeSubtab agentCode={FULL_RECORD} />)
    expect(screen.getByText('Bash')).toBeInTheDocument()
  })

  it('renders timestamps as <time> elements', () => {
    render(<AgentCodeSubtab agentCode={FULL_RECORD} />)
    const timeTags = document.querySelectorAll('time')
    const dateTimes = Array.from(timeTags).map((t) => t.getAttribute('dateTime'))
    expect(dateTimes).toContain('2026-05-22T10:00:00Z')
    expect(dateTimes).toContain('2026-05-22T10:05:30Z')
  })
})
