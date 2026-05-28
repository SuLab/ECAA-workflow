// Coverage: kind-keyed style/color helpers + the METRICS_POLL_MS export.

import { render } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import {
  METRICS_POLL_MS,
  PlaceholderPane,
  backgroundForKind,
  borderForKind,
  labelForKind,
  textForKind,
} from './common'

describe('common kind helpers', () => {
  it('PlaceholderPane renders its children', () => {
    const { getByText } = render(<PlaceholderPane>nothing here</PlaceholderPane>)
    expect(getByText('nothing here')).toBeInTheDocument()
  })

  it('color helpers map known kinds to their semantic CSS vars', () => {
    expect(backgroundForKind('task_started')).toMatch(/var\(--color-info-bg\)/)
    expect(borderForKind('task_completed')).toMatch(/var\(--color-success-border\)/)
    expect(textForKind('task_failed')).toMatch(/var\(--color-danger-fg\)/)
    expect(backgroundForKind('task_blocked')).toMatch(/var\(--color-warning-bg\)/)
  })

  it('color helpers fall back to neutral surface vars for unknown kinds', () => {
    expect(backgroundForKind('mystery_kind')).toMatch(/var\(--color-surface-0\)/)
    expect(borderForKind('mystery_kind')).toMatch(/var\(--color-border-default\)/)
    expect(textForKind('mystery_kind')).toMatch(/var\(--color-text-secondary\)/)
  })

  it('labelForKind humanizes known kinds and echoes unknown kinds', () => {
    expect(labelForKind('task_started')).toBe('Started')
    expect(labelForKind('task_completed')).toBe('Done')
    expect(labelForKind('task_failed')).toBe('Failed')
    expect(labelForKind('execution_finished')).toBe('Finished')
    expect(labelForKind('mystery_kind')).toBe('mystery_kind')
  })

  it('METRICS_POLL_MS is the documented 4-second cadence', () => {
    expect(METRICS_POLL_MS).toBe(4000)
  })
})
