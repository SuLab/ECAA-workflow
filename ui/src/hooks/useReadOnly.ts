// Detect a `share_token` query param on the current URL; when
// present, the UI lives in read-only mode: mutation affordances
// disable with a tooltip.

export function readOnlyShareToken(): string | null {
  if (typeof window === 'undefined') return null
  const params = new URLSearchParams(window.location.search)
  const tok = params.get('share_token')
  return tok && tok.length > 0 ? tok : null
}

export function isReadOnly(): boolean {
  return readOnlyShareToken() !== null
}
