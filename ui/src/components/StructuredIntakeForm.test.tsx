import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import * as chatClient from '../api/chatClient'
import StructuredIntakeForm from './StructuredIntakeForm'

vi.mock('../api/chatClient', () => ({
  getChatConfig: vi.fn(),
}))

describe('StructuredIntakeForm runtime catalog', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.mocked(chatClient.getChatConfig).mockResolvedValue({
      auto_title_enabled: false,
      auto_title_min_turns: 2,
      modalities: [
        {
          id: 'river_discharge_forecast',
          display_name: 'River discharge forecast',
        },
        { id: 'variant_calling', display_name: 'Variant calling' },
      ],
    })
  })

  it('accepts a runtime modality and an explicit starting data product', async () => {
    const onSubmit = vi.fn().mockResolvedValue(undefined)
    const user = userEvent.setup()
    render(<StructuredIntakeForm onSubmit={onSubmit} />)

    await waitFor(() => {
      const option = document.querySelector(
        'datalist option[value="river_discharge_forecast"]',
      )
      expect(option).not.toBeNull()
    })

    await user.clear(
      screen.getByRole('combobox', { name: /modality or analysis family/i }),
    )
    await user.type(
      screen.getByRole('combobox', { name: /modality or analysis family/i }),
      'river_discharge_forecast',
    )
    await user.type(
      screen.getByRole('textbox', { name: /what are you trying to find out/i }),
      'Forecast streamflow for every registered gauge.',
    )
    await user.type(
      screen.getByRole('textbox', {
        name: /registered starting data product/i,
      }),
      'gauge time-series table',
    )
    await user.click(screen.getByRole('button', { name: /start workflow/i }))

    expect(onSubmit).toHaveBeenCalledWith({
      goal: 'Forecast streamflow for every registered gauge.',
      modality: 'river_discharge_forecast',
      organism: '',
      input_data_stage: 'gauge time-series table',
      desired_outputs: '',
      uncertainties: '',
    })
  })
})
