import { describe, it, expect } from 'vitest'
import { sanitizeForSme } from './smeText'

describe('sanitizeForSme', () => {
  it('translates stage-ID prefix forms', () => {
    expect(sanitizeForSme('Waiting for discover_normalization to finish')).toBe(
      'Waiting for Normalization to finish',
    )
  })

  it('translates multiple stage-IDs in one string', () => {
    expect(
      sanitizeForSme(
        'discover_batch_correction produced an output; validate_qc is next',
      ),
    ).toBe('Batch correction produced an output; Qc is next')
  })

  it('strips runtime path fragments', () => {
    expect(
      sanitizeForSme(
        'Full decision at runtime/outputs/discover_normalization/decision.json',
      ),
    ).toBe('Full decision at the result file')
  })

  it('translates executor vocabulary', () => {
    expect(sanitizeForSme('The harness reported a failure')).toBe(
      'The system reported a failure',
    )
  })

  it('handles multiple replacements in a single string', () => {
    expect(
      sanitizeForSme(
        'The harness hasn\'t posted a tool_call to the Jobs tab yet',
      ),
    ).toBe("The system hasn't posted a action to the progress panel yet")
  })

  it('is idempotent', () => {
    const s = 'discover_normalization done; check runtime/logs/task.jsonl'
    const once = sanitizeForSme(s)
    expect(sanitizeForSme(once)).toBe(once)
  })

  it('passes clean prose through unchanged', () => {
    const clean = 'Normalization method selection is complete.'
    expect(sanitizeForSme(clean)).toBe(clean)
  })

  it('handles the empty string', () => {
    expect(sanitizeForSme('')).toBe('')
  })
})
