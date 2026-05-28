import { useEffect, useState } from 'react'
import {
  generateSshKey,
  getGitConfig,
  putGitConfig,
  testGitConnection,
  type GitConfig,
  type TestConnectionResponse,
} from '../../api/gitClient'
import { useTheme } from '../../hooks/useTheme'

/**
 * Git integration Settings page — admin-only.
 *
 * Provenance is per-package: every emitted package gets its own
 * `.git` directory automatically. This page configures the global
 * defaults (identity, master toggle, commit triggers, SSH key, remote
 * URL template, push behavior) that get applied to each new
 * per-package repo. Per-session inspection (status panel, commit log,
 * init/commit/push buttons) lives in the State Inspector's Provenance
 * view (future work) — not here.
 */
export default function GitSettingsPage({
  onClose,
}: {
  onClose: () => void
}): JSX.Element {
  const [cfg, setCfg] = useState<GitConfig | null>(null)
  const [testResult, setTestResult] = useState<TestConnectionResponse | null>(
    null,
  )
  const [publicKey, setPublicKey] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [info, setInfo] = useState<string | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const [sshKeyPathDraft, setSshKeyPathDraft] = useState<string>('')
  const { preference, setPreference } = useTheme()

  useEffect(() => {
    void (async () => {
      try {
        const loaded = await getGitConfig()
        setCfg(loaded)
        setSshKeyPathDraft(loaded.ssh_key_path ?? '')
      } catch (e) {
        setError(String(e))
      }
    })()
  }, [])

  if (!cfg) {
    return (
      <div style={{ padding: '1rem', color: 'var(--color-text-secondary)' }}>
        {error ?? 'Loading settings…'}
      </div>
    )
  }

  const disabled = !cfg.enabled
  const fade = disabled ? 0.4 : 1

  const patch = (next: Partial<GitConfig>) => {
    const merged = { ...cfg, ...next }
    setCfg(merged)
  }

  const save = async () => {
    setBusy('Saving')
    setError(null)
    setInfo(null)
    try {
      const merged: GitConfig = {
        ...cfg,
        ssh_key_path: sshKeyPathDraft.trim() || null,
      }
      const saved = await putGitConfig(merged)
      setCfg(saved)
      setInfo('Saved.')
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(null)
    }
  }

  const onTest = async () => {
    setBusy('Testing connection')
    setError(null)
    try {
      const r = await testGitConnection(cfg.remote_url ?? undefined)
      setTestResult(r)
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(null)
    }
  }

  const onGenerateKey = async () => {
    if (!sshKeyPathDraft.trim()) {
      setError('Provide a target path for the new key (must be under $HOME).')
      return
    }
    setBusy('Generating SSH key')
    setError(null)
    try {
      const r = await generateSshKey(sshKeyPathDraft.trim())
      setPublicKey(r.public_key)
      // Update config with the new key path + save so subsequent
      // `git push` invocations pick it up.
      const merged: GitConfig = {
        ...cfg,
        ssh_key_path: r.private_key_path,
      }
      const saved = await putGitConfig(merged)
      setCfg(saved)
      setSshKeyPathDraft(saved.ssh_key_path ?? '')
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(null)
    }
  }

  const onCopyPublicKey = async () => {
    if (!publicKey) return
    try {
      await navigator.clipboard.writeText(publicKey)
    } catch {
      // Best-effort — the textarea already exposes the key for
      // manual copy.
    }
  }

  const sectionStyle: React.CSSProperties = {
    border: '1px solid var(--color-border-default)',
    borderRadius: 8,
    padding: '1rem',
    background: 'var(--color-surface-1)',
    opacity: fade,
    pointerEvents: disabled ? 'none' : 'auto',
  }

  const labelStyle: React.CSSProperties = {
    display: 'flex',
    alignItems: 'center',
    gap: '0.5rem',
    marginBottom: '0.5rem',
    fontSize: '0.85rem',
    color: 'var(--color-text-secondary)',
  }

  const inputStyle: React.CSSProperties = {
    flex: 1,
    padding: '0.4rem 0.5rem',
    fontSize: '0.85rem',
    border: '1px solid var(--color-border-strong)',
    borderRadius: 4,
    fontFamily:
      'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
  }

  const buttonStyle: React.CSSProperties = {
    padding: '0.35rem 0.75rem',
    fontSize: '0.8rem',
    background: 'var(--color-accent)',
    color: 'var(--color-text-on-accent)',
    border: 'none',
    borderRadius: 4,
    cursor: 'pointer',
  }

  const ghostButton: React.CSSProperties = {
    ...buttonStyle,
    background: 'transparent',
    color: 'var(--color-accent)',
    border: '1px solid var(--color-accent)',
  }

  return (
    <div
      style={{
        flex: 1,
        overflowY: 'auto',
        padding: '1rem',
        background: 'var(--color-surface-0)',
      }}
    >
      <div
        style={{
          display: 'flex',
          alignItems: 'center',
          marginBottom: '1rem',
          gap: '0.5rem',
        }}
      >
        <button type="button" onClick={onClose} style={ghostButton}>
          ← Back to chat
        </button>
        <h1
          style={{
            fontSize: '1.1rem',
            fontWeight: 700,
            color: 'var(--color-text-primary)',
            margin: 0,
          }}
        >
          Settings
        </h1>
        <span style={{ flex: 1 }} />
        <button
          type="button"
          onClick={() => void save()}
          disabled={busy !== null}
          style={buttonStyle}
        >
          {busy === 'Saving' ? 'Saving…' : 'Save'}
        </button>
      </div>

      {error && (
        <div
          role="alert"
          style={{
            padding: '0.5rem 0.75rem',
            marginBottom: '1rem',
            background: 'var(--color-danger-bg)',
            border: '1px solid var(--color-danger-border)',
            color: 'var(--color-danger-fg)',
            borderRadius: 6,
            fontSize: '0.85rem',
          }}
        >
          {error}
        </div>
      )}
      {info && (
        <div
          role="status"
          style={{
            padding: '0.5rem 0.75rem',
            marginBottom: '1rem',
            background: 'var(--color-success-bg)',
            border: '1px solid var(--color-success-border)',
            color: 'var(--color-success-fg)',
            borderRadius: 6,
            fontSize: '0.85rem',
          }}
        >
          {info}
        </div>
      )}

      <div
        style={{
          display: 'flex',
          flexDirection: 'column',
          gap: '1rem',
          maxWidth: 720,
        }}
      >
        {/* Appearance — independent of git, always enabled */}
        <div
          style={{
            border: '1px solid var(--color-border-default)',
            borderRadius: 8,
            padding: '1rem',
            background: 'var(--color-surface-1)',
          }}
        >
          <h2
            style={{
              fontSize: '0.9rem',
              fontWeight: 600,
              color: 'var(--color-text-primary)',
              margin: '0 0 0.75rem',
            }}
          >
            Appearance
          </h2>
          <div
            role="radiogroup"
            aria-label="Theme"
            style={{ display: 'flex', gap: '0.5rem', flexWrap: 'wrap' }}
          >
            {(['light', 'dark', 'system'] as const).map((opt) => {
              const active = preference === opt
              const label =
                opt === 'system' ? 'System' : opt === 'light' ? 'Light' : 'Dark'
              return (
                <label
                  key={opt}
                  style={{
                    display: 'inline-flex',
                    alignItems: 'center',
                    gap: '0.4rem',
                    cursor: 'pointer',
                    padding: '0.35rem 0.75rem',
                    border: `1px solid ${active ? 'var(--color-accent-muted-border)' : 'var(--color-border-default)'}`,
                    borderRadius: 999,
                    background: active
                      ? 'var(--color-accent-muted-bg)'
                      : 'transparent',
                    color: active
                      ? 'var(--color-accent)'
                      : 'var(--color-text-secondary)',
                    fontSize: '0.8rem',
                    fontWeight: 500,
                  }}
                >
                  <input
                    type="radio"
                    name="theme"
                    value={opt}
                    checked={active}
                    onChange={() => setPreference(opt)}
                    style={{ margin: 0 }}
                  />
                  {label}
                </label>
              )
            })}
          </div>
          <p
            style={{
              margin: '0.5rem 0 0',
              fontSize: '0.78rem',
              color: 'var(--color-text-muted)',
            }}
          >
            System follows your operating-system light/dark setting; Light and Dark override it.
          </p>
        </div>

        {/* Per-package provenance info */}
        <div
          style={{
            border: '1px solid var(--color-border-default)',
            borderRadius: 8,
            padding: '1rem',
            background: 'var(--color-surface-1)',
          }}
        >
          <h2
            style={{
              fontSize: '0.9rem',
              fontWeight: 600,
              color: 'var(--color-text-primary)',
              margin: '0 0 0.5rem',
            }}
          >
            Per-package provenance
          </h2>
          <p
            style={{
              margin: 0,
              fontSize: '0.82rem',
              color: 'var(--color-text-secondary)',
              lineHeight: 1.4,
            }}
          >
            <strong>Provenance is per-package since 2026-05-12.</strong> Every
            emitted package gets its own <code>.git</code> directory
            automatically. Per-session inspection lives in the State
            Inspector's Provenance view (future work). The settings below are
            global defaults that get applied to each new per-package repo.
          </p>
        </div>

        {/* Top-level enable checkbox */}
        <div style={{ ...sectionStyle, opacity: 1, pointerEvents: 'auto' }}>
          <label style={{ ...labelStyle, marginBottom: 0, cursor: 'pointer' }}>
            <input
              type="checkbox"
              data-testid="git-enable"
              checked={cfg.enabled}
              onChange={(e) => patch({ enabled: e.target.checked })}
              style={{ width: 16, height: 16 }}
            />
            <span style={{ fontWeight: 600 }}>
              Enable git-backed provenance
            </span>
          </label>
          <p
            style={{
              margin: '0.5rem 0 0',
              fontSize: '0.78rem',
              color: 'var(--color-text-muted)',
            }}
          >
            When off, no git subprocess runs regardless of the settings below.
            The <code>ECAA_GIT_ENABLED=0</code> env var forces this off even
            when the checkbox is on.
          </p>
        </div>

        {/* Remote */}
        <div style={sectionStyle}>
          <h2
            style={{
              fontSize: '0.9rem',
              fontWeight: 600,
              color: 'var(--color-text-primary)',
              margin: '0 0 0.75rem',
            }}
          >
            Remote
          </h2>
          <div style={labelStyle}>
            <span style={{ width: 110 }}>Remote URL</span>
            <input
              style={inputStyle}
              placeholder="git@github.com:alan/provenance.git"
              value={cfg.remote_url ?? ''}
              onChange={(e) =>
                patch({ remote_url: e.target.value.trim() || null })
              }
            />
          </div>
          <p
            style={{
              margin: '0 0 0.5rem',
              fontSize: '0.78rem',
              color: 'var(--color-text-muted)',
            }}
          >
            Applied to each new per-package repo at creation time. Leave blank
            for local-only provenance.
          </p>
          <div style={{ display: 'flex', gap: '0.5rem' }}>
            <button type="button" onClick={() => void onTest()} style={ghostButton}>
              Test connection
            </button>
            {testResult && (
              <span
                style={{
                  alignSelf: 'center',
                  color: testResult.reachable ? 'var(--color-success-accent)' : 'var(--color-danger-fg)',
                  fontSize: '0.78rem',
                }}
              >
                {testResult.reachable
                  ? '✓ reachable'
                  : `✗ ${testResult.error ?? 'unreachable'}`}
              </span>
            )}
          </div>
        </div>

        {/* SSH credentials */}
        <div style={sectionStyle}>
          <h2
            style={{
              fontSize: '0.9rem',
              fontWeight: 600,
              color: 'var(--color-text-primary)',
              margin: '0 0 0.75rem',
            }}
          >
            SSH credentials
          </h2>
          <div style={labelStyle}>
            <span style={{ width: 110 }}>Key path</span>
            <input
              style={inputStyle}
              placeholder="/home/alan/.ssh/id_ed25519_scripps"
              value={sshKeyPathDraft}
              onChange={(e) => setSshKeyPathDraft(e.target.value)}
            />
          </div>
          <div style={{ display: 'flex', gap: '0.5rem' }}>
            <button
              type="button"
              onClick={() => void onGenerateKey()}
              style={ghostButton}
            >
              Generate new key
            </button>
          </div>
          {publicKey && (
            <div style={{ marginTop: '0.75rem' }}>
              <label
                style={{
                  display: 'block',
                  fontSize: '0.78rem',
                  color: 'var(--color-text-muted)',
                  marginBottom: '0.25rem',
                }}
              >
                Public key — paste this in your git host's Deploy Keys:
              </label>
              <textarea
                readOnly
                value={publicKey}
                style={{
                  width: '100%',
                  minHeight: 80,
                  fontFamily:
                    'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
                  fontSize: '0.75rem',
                  padding: '0.5rem',
                  border: '1px solid var(--color-border-strong)',
                  borderRadius: 4,
                }}
              />
              <button
                type="button"
                onClick={() => void onCopyPublicKey()}
                style={{ ...ghostButton, marginTop: '0.35rem' }}
              >
                Copy public key
              </button>
            </div>
          )}
        </div>

        {/* Commit triggers */}
        <div style={sectionStyle}>
          <h2
            style={{
              fontSize: '0.9rem',
              fontWeight: 600,
              color: 'var(--color-text-primary)',
              margin: '0 0 0.75rem',
            }}
          >
            Commit triggers
          </h2>
          <label style={labelStyle}>
            <input
              type="checkbox"
              checked={cfg.commit_on_emit}
              onChange={(e) => patch({ commit_on_emit: e.target.checked })}
            />
            <span>Commit on emit</span>
          </label>
          <label style={labelStyle}>
            <input
              type="checkbox"
              checked={cfg.commit_on_amend}
              onChange={(e) => patch({ commit_on_amend: e.target.checked })}
            />
            <span>Commit on amendment</span>
          </label>
          <label style={labelStyle}>
            <input
              type="checkbox"
              checked={cfg.commit_on_task_completed}
              onChange={(e) =>
                patch({ commit_on_task_completed: e.target.checked })
              }
            />
            <span>Commit on task completion (noisy)</span>
          </label>
          <label style={labelStyle}>
            <input
              type="checkbox"
              checked={cfg.auto_push}
              onChange={(e) => patch({ auto_push: e.target.checked })}
            />
            <span>Auto-push after commit</span>
          </label>
          <div style={labelStyle}>
            <span style={{ width: 110 }}>Push timeout (s)</span>
            <input
              style={inputStyle}
              type="number"
              min={1}
              max={600}
              value={cfg.push_timeout_secs}
              onChange={(e) => {
                const parsed = Number.parseInt(e.target.value, 10)
                if (!Number.isFinite(parsed)) return
                patch({ push_timeout_secs: parsed })
              }}
            />
          </div>
          <div style={labelStyle}>
            <span style={{ width: 110 }}>Author name</span>
            <input
              style={inputStyle}
              value={cfg.author_name}
              onChange={(e) => patch({ author_name: e.target.value })}
            />
          </div>
          <div style={labelStyle}>
            <span style={{ width: 110 }}>Author email</span>
            <input
              style={inputStyle}
              value={cfg.author_email}
              onChange={(e) => patch({ author_email: e.target.value })}
            />
          </div>
        </div>
      </div>
    </div>
  )
}
