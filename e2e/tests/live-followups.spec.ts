import { expect, test } from '@playwright/test'
import { chooseLiveFollowup } from '../helpers/liveFollowups'

test.describe('live follow-up selection', () => {
  test('chooses the first unspent scenario reply whose trigger matches the latest assistant text', () => {
    const picked = chooseLiveFollowup(
      [
        { trigger: '(?i)(organism|species)', reply: 'Drosophila melanogaster.' },
        { trigger: '(?i)(file|format|path)', reply: 'Files are staged locally.' },
      ],
      'Thanks. What organism or species should I use?',
      new Set(),
      'Please prepare the confirmation card.',
    )

    expect(picked).toEqual({
      kind: 'scenario',
      key: '(?i)(organism|species)',
      reply: 'Drosophila melanogaster.',
    })
  })

  test('falls back instead of repeating a scenario reply that was already sent', () => {
    const picked = chooseLiveFollowup(
      [{ trigger: '(?i)(organism|species)', reply: 'Drosophila melanogaster.' }],
      'Can you confirm the species?',
      new Set(['(?i)(organism|species)']),
      'Please prepare the confirmation card.',
    )

    expect(picked).toEqual({
      kind: 'fallback',
      key: 'fallback',
      reply: 'Please prepare the confirmation card.',
    })
  })
})
