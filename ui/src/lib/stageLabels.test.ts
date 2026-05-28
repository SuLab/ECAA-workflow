import { describe, it, expect } from 'vitest'
import { stageIdToLabel } from './stageLabels'

describe('stageIdToLabel', () => {
  it('strips the discover_ prefix', () => {
    expect(stageIdToLabel('discover_normalization')).toBe('Normalization')
  })

  it('strips the validate_ prefix', () => {
    expect(stageIdToLabel('validate_qc')).toBe('Qc')
  })

  it('strips the select_ prefix', () => {
    expect(stageIdToLabel('select_aligner')).toBe('Aligner')
  })

  it('humanizes underscore-separated bases', () => {
    expect(stageIdToLabel('discover_batch_correction')).toBe('Batch correction')
  })

  it('passes through ids without a known prefix', () => {
    expect(stageIdToLabel('alignment')).toBe('Alignment')
  })

  it('treats the empty string as empty', () => {
    expect(stageIdToLabel('')).toBe('')
  })

  it('does not strip unknown prefixes', () => {
    expect(stageIdToLabel('emit_package')).toBe('Emit package')
  })

  it('yields empty string for a bare prefix', () => {
    expect(stageIdToLabel('discover_')).toBe('')
  })
})
