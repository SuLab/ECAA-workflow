// Coverage: placeholder when no cross-version report; renders summary
// rows once present. The full report shape is opaque (`unknown`) so
// the test only asserts what the pane structurally exposes.

import { render } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { HistoryPane } from './HistoryTab'

describe('HistoryPane', () => {
  it('renders placeholder when crossVersionReport is null', () => {
    const { container } = render(<HistoryPane crossVersionReport={null} />)
    // Either a placeholder pane or an empty-state message; both should
    // be findable via text content.
    expect(container.textContent ?? '').not.toBe('')
  })

  it('renders without throwing when given an arbitrary report payload', () => {
    const report = {
      parent_package: '/p',
      child_package: '/c',
      tables: [],
      figures: [],
    }
    const { container } = render(<HistoryPane crossVersionReport={report} />)
    expect(container.textContent ?? '').not.toBe('')
  })
})
