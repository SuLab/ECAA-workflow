// Wrappers around /api/git/*. Mirrors the Rust handlers in
// crates/server/src/git_routes/mod.rs; response shapes are
// hand-maintained (not ts-rs) because the git module is a plain
// server concern with no typed DAG exports.
//
// Contract change: provenance is per-package. The global
// `repo_path` field is gone from `GitConfig` — every emitted package
// gets its own `.git` directory. Session-scoped operations
// (init/status/log/commit/push/remote) now address a session id and
// route through `/api/git/session/:id/*`. The SSH-key generator moved
// to `/api/git/keys/ssh`. `getGitConfig` / `putGitConfig` and
// `testGitConnection` remain global.
//
// Uses `jsonFetch` / `voidFetch` from `_fetch.ts` so every endpoint
// gains the share-token + auth-token header decoration +
// `AbortSignal` forwarding for free.

import { jsonFetch, voidFetch } from './_fetch'

export interface GitConfig {
  enabled: boolean
  remote_url: string | null
  ssh_key_path: string | null
  author_name: string
  author_email: string
  commit_on_emit: boolean
  commit_on_amend: boolean
  commit_on_task_completed: boolean
  auto_push: boolean
  push_timeout_secs: number
}

export interface GitStatus {
  repo_path: string
  remote_url: string | null
  git_available: boolean
  initialized: boolean
  last_commit: {
    sha: string
    subject: string
    committed_at: number
  } | null
  dirty_count: number
  commit_count: number
}

export interface GitLogEntry {
  sha: string
  subject: string
  committed_at: number
}

export interface GenerateSshKeyResponse {
  private_key_path: string
  public_key: string
}

export interface TestConnectionResponse {
  reachable: boolean
  error: string | null
}

export async function getGitConfig(): Promise<GitConfig> {
  return jsonFetch<GitConfig>('/api/git/config')
}

export async function putGitConfig(cfg: GitConfig): Promise<GitConfig> {
  return jsonFetch<GitConfig>('/api/git/config', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(cfg),
  })
}

export async function initGitRepo(sessionId: string): Promise<void> {
  return voidFetch(
    `/api/git/session/${encodeURIComponent(sessionId)}/init`,
    { method: 'POST' },
  )
}

export async function generateSshKey(
  path: string,
  comment?: string,
): Promise<GenerateSshKeyResponse> {
  return jsonFetch<GenerateSshKeyResponse>('/api/git/keys/ssh', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ path, comment }),
  })
}

export async function testGitConnection(
  remote_url?: string,
): Promise<TestConnectionResponse> {
  return jsonFetch<TestConnectionResponse>('/api/git/test-connection', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ remote_url: remote_url ?? null }),
  })
}

export async function getGitStatus(sessionId: string): Promise<GitStatus> {
  return jsonFetch<GitStatus>(
    `/api/git/session/${encodeURIComponent(sessionId)}/status`,
  )
}

export async function getGitLog(
  sessionId: string,
  limit = 20,
): Promise<GitLogEntry[]> {
  return jsonFetch<GitLogEntry[]>(
    `/api/git/session/${encodeURIComponent(sessionId)}/log?limit=${encodeURIComponent(limit)}`,
  )
}

export async function postGitCommit(
  sessionId: string,
  message: string,
  push: boolean,
  paths: string[] = [],
): Promise<{ sha: string; pushed: boolean }> {
  return jsonFetch<{ sha: string; pushed: boolean }>(
    `/api/git/session/${encodeURIComponent(sessionId)}/commit`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ message, push, paths }),
    },
  )
}

export async function postGitPush(sessionId: string): Promise<void> {
  return voidFetch(
    `/api/git/session/${encodeURIComponent(sessionId)}/push`,
    { method: 'POST' },
  )
}

export async function postSessionRemote(
  sessionId: string,
  url: string | null,
): Promise<void> {
  return voidFetch(
    `/api/git/session/${encodeURIComponent(sessionId)}/remote`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ remote_url: url }),
    },
  )
}
