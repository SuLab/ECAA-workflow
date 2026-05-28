/**
 * Inline markdown viewer for emitted-package artifacts (final_report.md,
 * task-level narratives, etc.). Fetches a path-jailed `/artifacts/`
 * URL and renders the body via `react-markdown` + `remark-gfm` (GFM
 * tables, task lists, strikethrough, autolinks).
 *
 * Open as a modal overlay over the Documents pane; close via the ×
 * button, Escape key, or backdrop click. Render errors degrade
 * gracefully to a "couldn't load this report" hint + a link to the
 * raw markdown so the SME can still get the content.
 */

import { useEffect, useRef, useState } from 'react'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'

interface Props {
  url: string
  /** Filename shown in the modal title bar + Download button name. */
  filename: string
  /** Caller closes the modal. */
  onClose: () => void
}

export function MarkdownViewer({ url, filename, onClose }: Props): JSX.Element {
  const [body, setBody] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const dialogRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    let cancelled = false
    setBody(null)
    setError(null)
    fetch(url, { credentials: 'same-origin' })
      .then(async (r) => {
        if (cancelled) return
        if (!r.ok) {
          setError(`${r.status} ${r.statusText}`)
          return
        }
        const text = await r.text()
        if (!cancelled) setBody(text)
      })
      .catch((e: unknown) => {
        if (!cancelled) setError((e as Error).message)
      })
    return () => {
      cancelled = true
    }
  }, [url])

  // Escape-to-close.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose])

  // Focus the dialog on mount so screen readers announce + Esc works
  // even when focus was elsewhere.
  useEffect(() => {
    dialogRef.current?.focus()
  }, [])

  return (
    <div
      role="presentation"
      onClick={onClose}
      style={backdropStyle}
      data-testid="markdown-viewer-backdrop"
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-label={`Inline preview of ${filename}`}
        tabIndex={-1}
        ref={dialogRef}
        onClick={(e) => e.stopPropagation()}
        style={dialogStyle}
        data-testid="markdown-viewer-dialog"
      >
        <header style={headerStyle}>
          <h2 style={titleStyle}>{filename}</h2>
          <div style={{ display: 'flex', gap: '0.5rem' }}>
            <a
              href={url}
              download={filename}
              style={iconButtonStyle}
              aria-label="Download as markdown"
              title="Download as .md"
            >
              ↓
            </a>
            <button
              type="button"
              onClick={onClose}
              style={iconButtonStyle}
              aria-label="Close preview"
              title="Close (Esc)"
            >
              ×
            </button>
          </div>
        </header>
        <div style={bodyStyle}>
          {body === null && !error && (
            <p style={emptyStyle}>Loading…</p>
          )}
          {error && (
            <p style={emptyStyle}>
              Couldn't load this report ({error}).{' '}
              <a href={url} target="_blank" rel="noreferrer noopener">
                Open raw
              </a>
            </p>
          )}
          {body && (
            <ReactMarkdown
              remarkPlugins={[remarkGfm]}
              components={{
                // Lift the default react-markdown styling into the
                // app's CSS variables so light/dark themes match.
                code({ className, children, ...props }) {
                  return (
                    <code className={className} style={codeStyle} {...props}>
                      {children}
                    </code>
                  )
                },
                table({ children }) {
                  return <table style={tableStyle}>{children}</table>
                },
                th({ children }) {
                  return <th style={thStyle}>{children}</th>
                },
                td({ children }) {
                  return <td style={tdStyle}>{children}</td>
                },
              }}
            >
              {body}
            </ReactMarkdown>
          )}
        </div>
      </div>
    </div>
  )
}

const backdropStyle: React.CSSProperties = {
  position: 'fixed',
  inset: 0,
  background: 'rgba(0, 0, 0, 0.55)',
  display: 'flex',
  alignItems: 'center',
  justifyContent: 'center',
  zIndex: 1000,
  padding: '2rem',
}

const dialogStyle: React.CSSProperties = {
  width: 'min(960px, 95vw)',
  maxHeight: '92vh',
  display: 'flex',
  flexDirection: 'column',
  background: 'var(--color-surface-0, #fff)',
  color: 'var(--color-text-primary)',
  border: '1px solid var(--color-border-subtle)',
  borderRadius: '0.5rem',
  boxShadow: '0 16px 56px rgba(0, 0, 0, 0.35)',
  outline: 'none',
}

const headerStyle: React.CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  justifyContent: 'space-between',
  padding: '0.7rem 1rem',
  borderBottom: '1px solid var(--color-border-subtle)',
  gap: '1rem',
}

const titleStyle: React.CSSProperties = {
  margin: 0,
  fontSize: '0.95rem',
  fontFamily: 'ui-monospace, monospace',
  fontWeight: 600,
  color: 'var(--color-text-primary)',
  overflow: 'hidden',
  textOverflow: 'ellipsis',
  whiteSpace: 'nowrap',
}

const iconButtonStyle: React.CSSProperties = {
  display: 'inline-flex',
  alignItems: 'center',
  justifyContent: 'center',
  width: '1.6rem',
  height: '1.6rem',
  background: 'transparent',
  border: '1px solid var(--color-border-subtle)',
  borderRadius: '0.3rem',
  color: 'var(--color-text-primary)',
  cursor: 'pointer',
  textDecoration: 'none',
  fontSize: '1.05rem',
  lineHeight: 1,
}

const bodyStyle: React.CSSProperties = {
  flex: 1,
  overflowY: 'auto',
  padding: '1rem 1.25rem',
  fontSize: '0.88rem',
  lineHeight: 1.5,
}

const emptyStyle: React.CSSProperties = {
  margin: 0,
  fontStyle: 'italic',
  color: 'var(--color-text-muted)',
}

const codeStyle: React.CSSProperties = {
  fontFamily: 'ui-monospace, monospace',
  background: 'var(--color-surface-muted)',
  padding: '0.05rem 0.3rem',
  borderRadius: '0.2rem',
  fontSize: '0.82rem',
}

const tableStyle: React.CSSProperties = {
  borderCollapse: 'collapse',
  width: '100%',
  margin: '0.6rem 0',
  fontSize: '0.82rem',
}

const thStyle: React.CSSProperties = {
  border: '1px solid var(--color-border-subtle)',
  padding: '0.35rem 0.6rem',
  textAlign: 'left',
  background: 'var(--color-surface-muted)',
  fontWeight: 600,
}

const tdStyle: React.CSSProperties = {
  border: '1px solid var(--color-border-subtle)',
  padding: '0.35rem 0.6rem',
  verticalAlign: 'top',
}
