// Inputs inspector tab — SME-supplied data registration surface.
//
// Two registration paths today:
// 1. Path reference: SME types/pastes a server-local directory path,
// clicks Validate; the server walks + hashes and persists into
// Session.inputs. Allowlist enforced server-side via SWFC_INPUT_ROOTS.
// 2. Upload (phase E placeholder): drag/drop UI is wired but the
// actual chunked upload backend is stubbed (returns 501). The UI
// surfaces the "coming soon" copy until phase E ships.
//
// The Inputs list shows registered sources with size + file count, and
// a Remove button that calls DELETE /inputs/:input_id (does not delete
// the underlying files). The list polls every POLL_MS so the panel
// reflects mutations made through the chat (set_intake_data_source
// tool, phase D).

import { useCallback, useEffect, useRef, useState } from 'react'
import {
  deleteInput,
  finalizeUpload,
  genUploadToken,
  listInputs,
  registerInputPath,
  uploadInputFile,
  type UserInput,
} from '../../api/chatClient'
import { CardContainer } from '../primitives/CardContainer'

const POLL_MS = 5_000

interface Props {
  sessionId: string | null
}

function humanBytes(n: number | bigint): string {
  const v = typeof n === 'bigint' ? Number(n) : n
  if (v < 1024) return `${v} B`
  if (v < 1024 * 1024) return `${(v / 1024).toFixed(1)} KB`
  if (v < 1024 * 1024 * 1024) return `${(v / 1024 / 1024).toFixed(1)} MB`
  return `${(v / 1024 / 1024 / 1024).toFixed(2)} GB`
}

function totalBytes(inp: UserInput): number {
  return inp.files.reduce(
    (s: number, f: { size_bytes: number | bigint }) => s + Number(f.size_bytes),
    0,
  )
}

interface UploadPanelProps {
  sessionId: string | null
  onUploadsRegistered: () => void
  onError: (msg: string) => void
  onInfo: (msg: string) => void
}

interface FileProgress {
  name: string
  total: number
  transferred: number
  state: 'queued' | 'uploading' | 'done' | 'error'
  err?: string
}

function UploadPanel({
  sessionId,
  onUploadsRegistered,
  onError,
  onInfo,
}: UploadPanelProps): JSX.Element {
  const [progress, setProgress] = useState<FileProgress[]>([])
  const [busy, setBusy] = useState(false)
  const inputRef = useRef<HTMLInputElement | null>(null)

  const handleFiles = useCallback(
    async (files: FileList | null) => {
      if (!sessionId) {
        onError('No active session yet — upload requires a session.')
        return
      }
      if (!files || files.length === 0) return
      const token = genUploadToken()
      const list: FileProgress[] = Array.from(files).map((f) => ({
        name: f.name,
        total: f.size,
        transferred: 0,
        state: 'queued',
      }))
      setProgress(list)
      setBusy(true)
      let completedAny = false
      for (let i = 0; i < files.length; i += 1) {
        const f = files[i]
        setProgress((prev) =>
          prev.map((p, idx) => (idx === i ? { ...p, state: 'uploading' } : p)),
        )
        try {
          await uploadInputFile(sessionId, f!, token, (transferred) => {
            setProgress((prev) =>
              prev.map((p, idx) =>
                idx === i ? { ...p, transferred } : p,
              ),
            )
          })
          setProgress((prev) =>
            prev.map((p, idx) =>
              idx === i ? { ...p, transferred: f!.size, state: 'done' } : p,
            ),
          )
          completedAny = true
        } catch (e) {
          setProgress((prev) =>
            prev.map((p, idx) =>
              idx === i
                ? {
                    ...p,
                    state: 'error',
                    err: (e as Error).message,
                  }
                : p,
            ),
          )
          onError(`Upload of ${f!.name} failed: ${(e as Error).message}`)
        }
      }
      if (completedAny) {
        try {
          await finalizeUpload(sessionId, token)
          onInfo(
            `Uploaded ${list.filter((p) => p.state !== 'error').length} of ${list.length} files.`,
          )
          onUploadsRegistered()
        } catch (e) {
          onError(`Finalizing batch failed: ${(e as Error).message}`)
        }
      }
      setBusy(false)
      if (inputRef.current) inputRef.current.value = ''
    },
    [sessionId, onError, onInfo, onUploadsRegistered],
  )

  return (
    <section
      aria-label="Upload files"
      data-testid="inputs-upload-panel"
      style={{
        padding: '0.75rem',
        border: '1px solid var(--color-border-default)',
        borderRadius: 6,
        background: 'var(--color-surface-0)',
        marginBottom: '0.85rem',
      }}
    >
      <h4 style={{ margin: '0 0 0.4rem', fontSize: '0.85rem' }}>
        Upload files from your laptop
      </h4>
      <p
        style={{
          margin: '0 0 0.55rem',
          fontSize: '0.72rem',
          color: 'var(--color-text-secondary)',
        }}
      >
        Multi-select supported. Each file is sliced into 8 MiB chunks
        and verified with a sha256 hash on the server. Single
        cohort-batch — once you press Upload, the whole selection is
        registered as one input.
      </p>
      <input
        ref={inputRef}
        type="file"
        multiple
        disabled={busy}
        onChange={(e) => void handleFiles(e.target.files)}
        aria-label="Choose files to upload"
        data-testid="inputs-upload-file-picker"
        style={{ marginBottom: '0.5rem', fontSize: '0.78rem' }}
      />
      {progress.length > 0 && (
        <ul
          aria-label="Upload progress"
          data-testid="inputs-upload-progress"
          style={{
            listStyle: 'none',
            padding: 0,
            margin: '0.5rem 0 0',
            display: 'flex',
            flexDirection: 'column',
            gap: '0.3rem',
          }}
        >
          {progress.map((p) => {
            const pct = p.total > 0 ? Math.round((p.transferred / p.total) * 100) : 0
            return (
              <li
                key={p.name}
                data-testid={`inputs-upload-row-${p.name}`}
                style={{
                  fontSize: '0.74rem',
                  fontFamily: 'ui-monospace, monospace',
                }}
              >
                <div
                  style={{
                    display: 'flex',
                    justifyContent: 'space-between',
                    gap: '0.5rem',
                  }}
                >
                  <span style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis' }}>
                    {p.name}
                  </span>
                  <span
                    style={{
                      color:
                        p.state === 'error'
                          ? 'var(--color-warning-fg)'
                          : p.state === 'done'
                            ? 'var(--color-success-fg)'
                            : 'var(--color-text-muted)',
                    }}
                  >
                    {p.state === 'error'
                      ? 'failed'
                      : p.state === 'done'
                        ? 'done'
                        : `${pct}%`}
                  </span>
                </div>
                <div
                  style={{
                    height: 4,
                    background: 'var(--color-chrome-bg-elevated)',
                    borderRadius: 2,
                    marginTop: 2,
                    overflow: 'hidden',
                  }}
                >
                  <div
                    style={{
                      height: '100%',
                      width: `${pct}%`,
                      background:
                        p.state === 'error'
                          ? 'var(--color-warning-fg)'
                          : 'var(--color-accent-fg)',
                      transition: 'width 0.2s',
                    }}
                  />
                </div>
              </li>
            )
          })}
        </ul>
      )}
    </section>
  )
}

export function InputsTab({ sessionId }: Props): JSX.Element {
  const [inputs, setInputs] = useState<UserInput[]>([])
  const [loading, setLoading] = useState(true)
  const [pathInput, setPathInput] = useState('')
  const [labelInput, setLabelInput] = useState('')
  const [registering, setRegistering] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [info, setInfo] = useState<string | null>(null)

  const refresh = useCallback(async () => {
    if (!sessionId) {
      setLoading(false)
      return
    }
    try {
      const list = await listInputs(sessionId)
      setInputs(list)
    } catch (e) {
      setError(`Failed to load inputs: ${(e as Error).message}`)
    } finally {
      setLoading(false)
    }
  }, [sessionId])

  useEffect(() => {
    void refresh()
    // Gate polling on document.visibilityState so a backgrounded tab
    // doesn't poll.
    const tick = () => {
      if (document.visibilityState !== 'visible') return
      void refresh()
    }
    const t = setInterval(tick, POLL_MS)
    return () => clearInterval(t)
  }, [refresh])

  const onRegister = useCallback(async () => {
    if (!sessionId) {
      setError('No active session yet.')
      return
    }
    if (!pathInput.trim()) {
      setError('Path is required.')
      return
    }
    setRegistering(true)
    setError(null)
    setInfo(null)
    try {
      const list = await registerInputPath(sessionId, {
        path: pathInput.trim(),
        label: labelInput.trim() || undefined,
      })
      setInputs(list)
      const last = list[list.length - 1]
      const fileCount = last?.files.length ?? 0
      const size = last ? humanBytes(totalBytes(last)) : ''
      setInfo(
        `Registered ${fileCount} file${fileCount === 1 ? '' : 's'} (${size}) from ${pathInput.trim()}`,
      )
      setPathInput('')
      setLabelInput('')
    } catch (e) {
      setError((e as Error).message)
    } finally {
      setRegistering(false)
    }
  }, [sessionId, pathInput, labelInput])

  const onRemove = useCallback(
    async (inputId: string) => {
      if (!sessionId) return
      try {
        const list = await deleteInput(sessionId, inputId)
        setInputs(list)
        setError(null)
        setInfo('Removed input registration (underlying files left in place).')
      } catch (e) {
        setError((e as Error).message)
      }
    },
    [sessionId],
  )

  return (
    <div
      aria-label="Inputs panel"
      data-testid="inputs-tab"
      style={{
        flex: 1,
        overflowY: 'auto',
        padding: '1rem',
        background: 'var(--color-surface-1)',
        color: 'var(--color-text-primary)',
      }}
    >
      <h3 style={{ margin: '0 0 0.4rem', fontSize: '0.95rem' }}>
        Your data inputs
      </h3>
      <p
        style={{
          margin: '0 0 1rem',
          fontSize: '0.75rem',
          color: 'var(--color-text-secondary)',
        }}
      >
        Point the planner at a directory of files already on this
        server, or upload files from your laptop. Registered inputs
        flow into the data-acquisition stage and appear in CONTEXT.md
        when the package is emitted.
      </p>

      {/* Path-reference registration */}
      <section
        aria-label="Register a server-local path"
        style={{
          padding: '0.75rem',
          border: '1px solid var(--color-border-default)',
          borderRadius: 6,
          background: 'var(--color-surface-0)',
          marginBottom: '0.85rem',
        }}
      >
        <h4 style={{ margin: '0 0 0.5rem', fontSize: '0.85rem' }}>
          Use a server-local path
        </h4>
        <p
          style={{
            margin: '0 0 0.55rem',
            fontSize: '0.72rem',
            color: 'var(--color-text-secondary)',
          }}
        >
          Absolute path inside an allowlisted root (default
          {' '}
          <code style={{ fontFamily: 'ui-monospace, monospace' }}>
            /home/&lt;you&gt;/data
          </code>
          ). The server walks the directory and computes per-file
          sha256 — that takes seconds for small cohorts and may take
          longer for multi-GB ones.
        </p>
        <div
          style={{
            display: 'flex',
            flexDirection: 'column',
            gap: '0.4rem',
          }}
        >
          <input
            type="text"
            placeholder="/home/you/data/2025-cohort"
            value={pathInput}
            onChange={(e) => setPathInput(e.target.value)}
            disabled={registering}
            aria-label="Server-local path"
            data-testid="inputs-path-field"
            style={{
              padding: '0.4rem 0.5rem',
              fontSize: '0.78rem',
              fontFamily: 'ui-monospace, monospace',
              border: '1px solid var(--color-border-default)',
              borderRadius: 4,
              background: 'var(--color-surface-1)',
              color: 'var(--color-text-primary)',
            }}
          />
          <input
            type="text"
            placeholder="Label (optional, defaults to directory name)"
            value={labelInput}
            onChange={(e) => setLabelInput(e.target.value)}
            disabled={registering}
            aria-label="Input label"
            data-testid="inputs-label-field"
            style={{
              padding: '0.4rem 0.5rem',
              fontSize: '0.78rem',
              border: '1px solid var(--color-border-default)',
              borderRadius: 4,
              background: 'var(--color-surface-1)',
              color: 'var(--color-text-primary)',
            }}
          />
          <button
            type="button"
            onClick={onRegister}
            disabled={registering || !pathInput.trim()}
            data-testid="inputs-register-button"
            style={{
              alignSelf: 'flex-start',
              padding: '0.4rem 0.85rem',
              fontSize: '0.78rem',
              border: '1px solid var(--color-accent-fg)',
              background: registering
                ? 'var(--color-chrome-bg-elevated)'
                : 'var(--color-accent-soft)',
              color: 'var(--color-accent-fg)',
              borderRadius: 4,
              cursor: registering ? 'wait' : 'pointer',
            }}
          >
            {registering ? 'Validating + hashing…' : 'Register path'}
          </button>
        </div>
      </section>

      <UploadPanel
        sessionId={sessionId}
        onUploadsRegistered={() => void refresh()}
        onError={(msg) => setError(msg)}
        onInfo={(msg) => setInfo(msg)}
      />


      {/* Inline status messages */}
      {error && (
        <CardContainer
          palette="warning"
          role="alert"
          dataAttrs={{ 'data-testid': 'inputs-error' }}
          style={{
            padding: '0.5rem 0.65rem',
            marginBottom: '0.6rem',
            marginTop: 0,
            color: 'var(--color-warning-fg)',
            fontSize: '0.78rem',
            borderLeft: '1px solid var(--color-warning-fg)',
          }}
        >
          {error}
        </CardContainer>
      )}
      {info && !error && (
        <CardContainer
          palette="success"
          role="status"
          dataAttrs={{ 'data-testid': 'inputs-info' }}
          style={{
            padding: '0.5rem 0.65rem',
            marginBottom: '0.6rem',
            marginTop: 0,
            color: 'var(--color-success-fg)',
            fontSize: '0.78rem',
            borderLeft: '1px solid var(--color-success-fg)',
          }}
        >
          {info}
        </CardContainer>
      )}

      {/* Registered inputs list */}
      <h4
        style={{
          margin: '1rem 0 0.4rem',
          fontSize: '0.85rem',
          color: 'var(--color-text-primary)',
        }}
      >
        Registered ({inputs.length})
      </h4>
      {loading && inputs.length === 0 && (
        <p
          style={{
            margin: 0,
            fontSize: '0.75rem',
            color: 'var(--color-text-muted)',
          }}
        >
          Loading…
        </p>
      )}
      {!loading && inputs.length === 0 && (
        <p
          style={{
            margin: 0,
            fontSize: '0.75rem',
            color: 'var(--color-text-muted)',
          }}
          data-testid="inputs-empty"
        >
          No inputs registered yet. The data-acquisition stage will
          fall back to public accessions captured in chat.
        </p>
      )}
      <ul
        style={{
          listStyle: 'none',
          padding: 0,
          margin: 0,
          display: 'flex',
          flexDirection: 'column',
          gap: '0.45rem',
        }}
      >
        {inputs.map((inp) => (
          <li
            key={inp.input_id}
            data-testid={`input-row-${inp.input_id}`}
            style={{
              padding: '0.55rem 0.7rem',
              border: '1px solid var(--color-border-default)',
              borderRadius: 4,
              fontSize: '0.78rem',
              background: 'var(--color-surface-0)',
            }}
          >
            <div
              style={{
                display: 'flex',
                justifyContent: 'space-between',
                alignItems: 'center',
                gap: '0.5rem',
                marginBottom: '0.2rem',
              }}
            >
              <span style={{ fontWeight: 600 }}>{inp.label}</span>
              <span
                aria-label={`Input source: ${inp.kind === 'local_path' ? 'local path' : 'uploaded'}`}
                style={{
                  fontSize: '0.65rem',
                  padding: '0.1rem 0.4rem',
                  background: 'var(--color-chrome-bg-elevated)',
                  color: 'var(--color-chrome-fg-muted)',
                  borderRadius: 999,
                }}
              >
                {inp.kind === 'local_path' ? 'Local path' : 'Uploaded'}
              </span>
            </div>
            <div
              style={{
                fontFamily: 'ui-monospace, monospace',
                fontSize: '0.7rem',
                color: 'var(--color-text-muted)',
                wordBreak: 'break-all',
                marginBottom: '0.25rem',
              }}
            >
              {inp.root_path}
            </div>
            <div
              style={{
                display: 'flex',
                gap: '0.6rem',
                fontSize: '0.7rem',
                color: 'var(--color-text-secondary)',
                alignItems: 'center',
              }}
            >
              <span>{inp.files.length} file{inp.files.length === 1 ? '' : 's'}</span>
              <span>{humanBytes(totalBytes(inp))}</span>
              <span style={{ flex: 1 }} />
              <button
                type="button"
                onClick={() => void onRemove(inp.input_id)}
                aria-label={`Remove ${inp.label}`}
                data-testid={`input-remove-${inp.input_id}`}
                style={{
                  fontSize: '0.7rem',
                  padding: '0.2rem 0.5rem',
                  border: '1px solid var(--color-border-default)',
                  background: 'transparent',
                  color: 'var(--color-text-secondary)',
                  borderRadius: 4,
                  cursor: 'pointer',
                }}
              >
                Remove
              </button>
            </div>
          </li>
        ))}
      </ul>
    </div>
  )
}
