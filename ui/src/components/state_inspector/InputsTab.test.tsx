import { beforeEach, describe, expect, it, vi } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'

import {
  finalizeUpload,
  genUploadToken,
  listInputs,
  uploadInputFile,
} from '../../api/chatClient'
import { InputsTab } from './InputsTab'

vi.mock('../../api/chatClient', () => ({
  deleteInput: vi.fn(),
  finalizeUpload: vi.fn(),
  genUploadToken: vi.fn(),
  listInputs: vi.fn(),
  registerInputPath: vi.fn(),
  uploadInputFile: vi.fn(),
}))

const mockFinalizeUpload = vi.mocked(finalizeUpload)
const mockGenUploadToken = vi.mocked(genUploadToken)
const mockListInputs = vi.mocked(listInputs)
const mockUploadInputFile = vi.mocked(uploadInputFile)

beforeEach(() => {
  vi.clearAllMocks()
  mockGenUploadToken.mockReturnValue('upload-token')
  mockListInputs.mockResolvedValue([])
  mockFinalizeUpload.mockResolvedValue(null)
})

describe('InputsTab', () => {
  it('clears an earlier upload error when a retry succeeds', async () => {
    const user = userEvent.setup()
    mockUploadInputFile
      .mockRejectedValueOnce(new Error('disk reserve guard tripped'))
      .mockResolvedValueOnce({ status: 'complete' })

    render(<InputsTab sessionId="session-1" />)
    const picker = screen.getByTestId('inputs-upload-file-picker')

    await user.upload(picker, new File(['first'], 'counts.tsv'))
    expect(await screen.findByTestId('inputs-error')).toHaveTextContent(
      'disk reserve guard tripped',
    )

    await user.upload(picker, new File(['retry'], 'counts.tsv'))

    await waitFor(() =>
      expect(screen.queryByTestId('inputs-error')).not.toBeInTheDocument(),
    )
    expect(screen.getByTestId('inputs-info')).toHaveTextContent(
      'Uploaded 1 of 1 files.',
    )
  })
})
