// V3+v4 residuals closure PromotionRefusedCard render tests.

import { render } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { PromotionRefusedCard } from './PromotionRefusedCard'
import type { RefusalReport } from '../../types/RefusalReport'

const promotionRefusal: RefusalReport = {
  id: 'promotion_refused',
  kind: { kind: 'promotion_refused' },
  statement:
    '2 nodes failed the validation × lifecycle promotion grid: missing clinical_lead approval',
  references: ['node_a', 'node_b'],
  unblock_paths: [
    {
      kind: 'escalate_to_reviewer',
      reviewer_class: 'clinical_lead',
      required_artifacts: ['validation_report.pdf'],
      target_outcome: 'draft_dag' as RefusalReport['unblock_paths'][number]['target_outcome'],
    },
  ],
}

describe('PromotionRefusedCard', () => {
  it('renders the refusal statement and node references', () => {
    const { getByText } = render(
      <PromotionRefusedCard refusal={promotionRefusal} />,
    )
    expect(getByText(/Promotion refused/i)).toBeInTheDocument()
    expect(
      getByText(/2 nodes failed the validation × lifecycle promotion grid/i),
    ).toBeInTheDocument()
    expect(getByText('node_a')).toBeInTheDocument()
    expect(getByText('node_b')).toBeInTheDocument()
  })

  it('renders the EscalateToReviewer paths grouped under "Required approvals"', () => {
    const { getByText } = render(
      <PromotionRefusedCard refusal={promotionRefusal} />,
    )
    expect(getByText(/Required approvals/i)).toBeInTheDocument()
    expect(getByText('clinical_lead')).toBeInTheDocument()
    expect(getByText(/validation_report\.pdf/i)).toBeInTheDocument()
  })

  it('renders nothing for non-PromotionRefused refusal kinds', () => {
    const otherRefusal: RefusalReport = {
      ...promotionRefusal,
      kind: { kind: 'license_missing' },
    }
    const { container } = render(<PromotionRefusedCard refusal={otherRefusal} />)
    expect(container.textContent).toBe('')
  })

  it('renders the "no recovery affordances" hint when unblock_paths is empty', () => {
    const noPaths: RefusalReport = { ...promotionRefusal, unblock_paths: [] }
    const { getByText } = render(<PromotionRefusedCard refusal={noPaths} />)
    expect(
      getByText(/No recovery affordances available/i),
    ).toBeInTheDocument()
  })
})
