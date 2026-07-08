import { beforeEach, expect, it, vi } from 'vitest'
import { getCapabilities, importPackage } from './chatClient'

const fetchMock = vi.fn()

beforeEach(() => {
  fetchMock.mockReset()
  vi.stubGlobal('fetch', fetchMock)
})

it('importPackage POSTs the raw File body to the package-import endpoint', async () => {
  fetchMock.mockResolvedValue({
    ok: true,
    json: async () => ({
      session_id: 's1',
      imported: true,
      capabilities: {
        tier_label: 'minimal_audit',
        explore: true,
        reverify: true,
        replay_tier1: true,
        replay_tier2: false,
        tabs: {},
      },
    }),
  })
  const file = new File([new Uint8Array([0x50, 0x4b, 0x03, 0x04])], 'pkg.zip', {
    type: 'application/zip',
  })
  const res = await importPackage(file)
  expect(res.session_id).toBe('s1')
  expect(res.imported).toBe(true)

  const [url, opts] = fetchMock.mock.calls[0]!
  // The fetch helper rewrites /api/chat/... to the versioned /api/v1/chat/...
  // prefix, so assert on the stable tail rather than the whole path.
  expect(String(url)).toContain('package/import')
  expect(opts.method).toBe('POST')
  expect(opts.body).toBe(file)
  expect(opts.headers['Content-Type']).toBe('application/octet-stream')
})

it('getCapabilities GETs the session capabilities probe', async () => {
  fetchMock.mockResolvedValue({
    ok: true,
    json: async () => ({
      imported: true,
      capabilities: {
        tier_label: 'minimal_audit',
        explore: true,
        reverify: true,
        replay_tier1: true,
        replay_tier2: false,
        tabs: {},
      },
    }),
  })
  const res = await getCapabilities('s1')
  expect(res.imported).toBe(true)
  expect(res.capabilities.replay_tier2).toBe(false)
  expect(String(fetchMock.mock.calls[0]![0])).toContain('session/s1/capabilities')
})
