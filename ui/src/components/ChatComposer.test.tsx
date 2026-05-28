// Freeform side of the freeform↔structured transition. Covers draft
// preservation across re-renders (a SME typing in the composer must not
// lose their draft when the assistant surfaces a StructuredCaptureTurnCard),
// plus submission behaviour, Shift+Enter newlines, and Enter submit. The
// structured side is covered in StructuredCaptureTurnCard.test.tsx.

import { describe, expect, it, vi } from 'vitest'
import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import ChatComposer from './ChatComposer'

describe('ChatComposer', () => {
  it('renders a labelled textarea and a Send button', () => {
    render(<ChatComposer onSend={vi.fn()} autoFocus={false} />)
    expect(screen.getByLabelText('Message')).toBeInTheDocument()
    expect(screen.getByLabelText(/send message/i)).toBeInTheDocument()
  })

  it('submits the trimmed text on Enter and clears the draft', async () => {
    const user = userEvent.setup()
    const onSend = vi.fn().mockResolvedValue(undefined)
    render(<ChatComposer onSend={onSend} autoFocus={false} />)
    const textarea = screen.getByLabelText('Message') as HTMLTextAreaElement
    await user.click(textarea)
    await user.keyboard('  hello world  {Enter}')
    expect(onSend).toHaveBeenCalledOnce()
    expect(onSend).toHaveBeenCalledWith('hello world')
    expect(textarea.value).toBe('')
  })

  it('inserts a newline on Shift+Enter without submitting', async () => {
    const user = userEvent.setup()
    const onSend = vi.fn()
    render(<ChatComposer onSend={onSend} autoFocus={false} />)
    const textarea = screen.getByLabelText('Message') as HTMLTextAreaElement
    await user.click(textarea)
    await user.keyboard('first line{Shift>}{Enter}{/Shift}second line')
    expect(textarea.value).toBe('first line\nsecond line')
    expect(onSend).not.toHaveBeenCalled()
  })

  it('preserves the in-progress draft across an unrelated re-render', async () => {
    // Transition guard: the SME types something, an unrelated prop
    // change forces a re-render (e.g. the parent's sending state
    // flips), and the draft must NOT reset.
    const user = userEvent.setup()
    const onSend = vi.fn()
    const { rerender } = render(
      <ChatComposer onSend={onSend} autoFocus={false} disabled={false} />,
    )
    const textarea = screen.getByLabelText('Message') as HTMLTextAreaElement
    await user.click(textarea)
    await user.keyboard('partial draft about scRNA-seq')
    expect(textarea.value).toBe('partial draft about scRNA-seq')
    // Force a re-render by toggling the disabled prop and back
    rerender(<ChatComposer onSend={onSend} autoFocus={false} disabled />)
    rerender(<ChatComposer onSend={onSend} autoFocus={false} disabled={false} />)
    // Same textarea should still hold the draft
    const textareaAfter = screen.getByLabelText('Message') as HTMLTextAreaElement
    expect(textareaAfter.value).toBe('partial draft about scRNA-seq')
  })

  it('disables the Send button when the draft is empty or whitespace', async () => {
    const user = userEvent.setup()
    render(<ChatComposer onSend={vi.fn()} autoFocus={false} />)
    expect(screen.getByLabelText(/send message/i)).toBeDisabled()
    await user.click(screen.getByLabelText('Message'))
    await user.keyboard('   ')
    // Whitespace-only counts as empty for the Send button enable check
    expect(screen.getByLabelText(/send message/i)).toBeDisabled()
    await user.keyboard('text')
    expect(screen.getByLabelText(/send message/i)).toBeEnabled()
  })

  it('disables both controls when the disabled prop is set', () => {
    render(<ChatComposer onSend={vi.fn()} disabled autoFocus={false} />)
    expect(screen.getByLabelText('Message')).toBeDisabled()
    expect(screen.getByLabelText(/send message/i)).toBeDisabled()
  })
})
