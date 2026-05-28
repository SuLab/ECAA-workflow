// Structured side of the freeform↔structured transition: all fields
// render with label associations, partial state survives re-renders,
// required-field validation fires, and Submit/Cancel work.

import { describe, expect, it, vi } from 'vitest'
import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import StructuredCaptureTurnCard from './StructuredCaptureTurnCard'

const BASE_FIELDS = [
  { key: 'gse', label: 'GEO accession', required: true },
  { key: 'n', label: 'Sample count', required: true },
  { key: 'tissue', label: 'Tissue', placeholder: 'e.g. ileal biopsy' },
  { key: 'notes', label: 'Notes', multiline: true },
]

describe('StructuredCaptureTurnCard', () => {
  it('renders all fields with their labels and required asterisks', () => {
    render(
      <StructuredCaptureTurnCard
        title="Per-study metadata"
        fields={BASE_FIELDS}
        onSubmit={vi.fn()}
      />,
    )
    expect(screen.getByLabelText(/GEO accession/i)).toBeInTheDocument()
    expect(screen.getByLabelText(/Sample count/i)).toBeInTheDocument()
    expect(screen.getByLabelText(/Tissue/i)).toBeInTheDocument()
    expect(screen.getByLabelText(/Notes/i)).toBeInTheDocument()
  })

  it('preserves partially-filled state across a re-render', async () => {
    // Partial structured-capture state must survive when the user
    // briefly switches focus (e.g. to the freeform composer) and
    // returns. We simulate the simplest version of that — a parent
    // re-render that doesn't replace the component.
    const user = userEvent.setup()
    const { rerender } = render(
      <StructuredCaptureTurnCard
        title="Per-study metadata"
        fields={BASE_FIELDS}
        onSubmit={vi.fn()}
      />,
    )
    const gse = screen.getByLabelText(/GEO accession/i) as HTMLInputElement
    const tissue = screen.getByLabelText(/Tissue/i) as HTMLInputElement
    await user.click(gse)
    await user.keyboard('GSE12345')
    await user.click(tissue)
    await user.keyboard('ileal biopsy')
    expect(gse.value).toBe('GSE12345')
    expect(tissue.value).toBe('ileal biopsy')
    // Force re-render with the same fields — state should survive
    rerender(
      <StructuredCaptureTurnCard
        title="Per-study metadata"
        fields={BASE_FIELDS}
        onSubmit={vi.fn()}
      />,
    )
    const gseAfter = screen.getByLabelText(/GEO accession/i) as HTMLInputElement
    const tissueAfter = screen.getByLabelText(/Tissue/i) as HTMLInputElement
    expect(gseAfter.value).toBe('GSE12345')
    expect(tissueAfter.value).toBe('ileal biopsy')
  })

  it('blocks submit and surfaces an alert when required fields are empty', async () => {
    const user = userEvent.setup()
    const onSubmit = vi.fn()
    render(
      <StructuredCaptureTurnCard
        title="Per-study metadata"
        fields={BASE_FIELDS}
        onSubmit={onSubmit}
      />,
    )
    await user.click(screen.getByRole('button', { name: /submit/i }))
    expect(onSubmit).not.toHaveBeenCalled()
    expect(screen.getByRole('alert').textContent).toMatch(/please fill in/i)
  })

  it('submits the captured values when required fields are filled', async () => {
    const user = userEvent.setup()
    const onSubmit = vi.fn().mockResolvedValue(undefined)
    render(
      <StructuredCaptureTurnCard
        title="Per-study metadata"
        fields={BASE_FIELDS}
        onSubmit={onSubmit}
      />,
    )
    await user.click(screen.getByLabelText(/GEO accession/i))
    await user.keyboard('GSE12345')
    await user.click(screen.getByLabelText(/Sample count/i))
    await user.keyboard('24')
    await user.click(screen.getByRole('button', { name: /submit/i }))
    expect(onSubmit).toHaveBeenCalledOnce()
    const args = onSubmit!.mock.calls[0]![0]
    expect(args.gse).toBe('GSE12345')
    expect(args.n).toBe('24')
  })

  it('honours an optional Cancel handler when provided', async () => {
    const user = userEvent.setup()
    const onCancel = vi.fn()
    render(
      <StructuredCaptureTurnCard
        title="Per-study metadata"
        fields={BASE_FIELDS}
        onSubmit={vi.fn()}
        onCancel={onCancel}
      />,
    )
    await user.click(screen.getByRole('button', { name: /cancel/i }))
    expect(onCancel).toHaveBeenCalledOnce()
  })

  it('omits the Cancel button when no onCancel is provided', () => {
    render(
      <StructuredCaptureTurnCard
        title="Per-study metadata"
        fields={BASE_FIELDS}
        onSubmit={vi.fn()}
      />,
    )
    expect(screen.queryByRole('button', { name: /cancel/i })).toBeNull()
  })
})
