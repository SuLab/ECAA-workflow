import { useEffect, useState } from 'react'
import { artifactUrl, type SessionStateSnapshot } from '../../api/chatClient'
import { PlaceholderPane } from './common'
import { MarkdownViewer } from './MarkdownViewer'

/**
 * URL for the streaming package download endpoint
 * (`GET /api/chat/session/:id/package.tar.gz`). Server builds the
 * archive in memory and returns it as an attachment named after the
 * emitted package directory.
 */
function packageTarballUrl(sessionId: string): string {
  return `/api/chat/session/${encodeURIComponent(sessionId)}/package.tar.gz`
}

/**
 * Documents tab. Surfaces the emitted package's files as openable
 * links via the path-jailed `/api/chat/session/:id/artifacts/*path`
 * endpoint. Three sections render once the session reaches `emitted`:
 *
 *   1. **Final report** — `runtime/outputs/final_reporting/final_report.md`
 *      gets a dedicated card with View (opens markdown in a new tab)
 *      and Download buttons. Auto-probed; absent when the
 *      final_reporting stage hasn't completed yet.
 *   2. **Package artifacts** — the four canonical top-level files
 *      (WORKFLOW.json, PROMPT.md, CONTEXT.md, ro-crate-metadata.json),
 *      each with a View link.
 *   3. **All per-task outputs** — `runtime/outputs/` directory listing
 *      so the SME can navigate to any per-stage artifact (figures,
 *      result.json, narrative, table files).
 *
 * Wires the `/artifacts` endpoint into the Documents tab so links open
 * directly in the browser instead of requiring the SME to find the
 * package directory on the host filesystem.
 */
export function DocumentsPane({
  state,
  sessionId,
}: {
  state: SessionStateSnapshot | null
  sessionId: string | null
}): JSX.Element {
  const emittedPath = state?.emitted_package_path
  const isEmitted = state?.state.kind === 'emitted'

  // Probe for the final report. The server returns 404 if absent;
  // we use a HEAD request equivalent (small GET, ignore the body) to
  // decide whether to show the card. Best-effort — failures degrade
  // to "card hidden", never a tab-level error.
  const [hasFinalReport, setHasFinalReport] = useState<boolean | null>(null)
  useEffect(() => {
    if (!sessionId || !isEmitted) {
      setHasFinalReport(null)
      return
    }
    let cancelled = false
    const url = artifactUrl(
      sessionId,
      'runtime/outputs/final_reporting/final_report.md',
    )
    fetch(url, { method: 'GET', credentials: 'same-origin' })
      .then((r) => {
        if (cancelled) return
        setHasFinalReport(r.ok)
      })
      .catch(() => {
        if (cancelled) return
        setHasFinalReport(false)
      })
    return () => {
      cancelled = true
    }
  }, [sessionId, isEmitted])

  // Target for the inline markdown viewer modal: which markdown artifact
  // is currently open. `null` keeps the modal closed. The FinalReportCard
  // and every markdown ArtifactRowView write into this same state so a
  // single MarkdownViewer instance handles all of them.
  const [viewer, setViewer] = useState<ViewerTarget | null>(null)

  if (!sessionId || !emittedPath || !isEmitted) {
    return (
      <PlaceholderPane>
        Documents the package emits will appear here after confirmation.
      </PlaceholderPane>
    )
  }

  const artifacts: ArtifactRow[] = [
    {
      name: 'WORKFLOW.json',
      description: 'DAG + task state',
      path: 'WORKFLOW.json',
      kind: 'opaque',
    },
    {
      name: 'PROMPT.md',
      description: 'Agent execution prompt',
      path: 'PROMPT.md',
      kind: 'markdown',
    },
    {
      name: 'CONTEXT.md',
      description: 'Intake context + policies',
      path: 'CONTEXT.md',
      kind: 'markdown',
    },
    {
      name: 'ro-crate-metadata.json',
      description: 'RO-Crate provenance',
      path: 'ro-crate-metadata.json',
      kind: 'opaque',
    },
  ]

  return (
    <div
      style={{
        flex: 1,
        overflowY: 'auto',
        padding: '1rem',
        background: 'var(--color-surface-1)',
      }}
      data-testid="state-documents-pane"
    >
      {hasFinalReport && (
        <FinalReportCard
          sessionId={sessionId}
          onOpenInline={() =>
            setViewer({
              path: 'runtime/outputs/final_reporting/final_report.md',
              filename: 'final_report.md',
            })
          }
        />
      )}
      {viewer && (
        <MarkdownViewer
          url={artifactUrl(sessionId, viewer.path)}
          filename={viewer.filename}
          onClose={() => setViewer(null)}
        />
      )}

      <div
        style={{
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'space-between',
          gap: '1rem',
          margin: hasFinalReport ? '1rem 0 0.5rem' : '0 0 0.5rem',
        }}
      >
        <h3
          style={{
            margin: 0,
            fontSize: '0.95rem',
            color: 'var(--color-text-primary)',
          }}
        >
          Emitted package
        </h3>
        <a
          href={packageTarballUrl(sessionId)}
          download
          style={secondaryButtonStyle}
          aria-label="Download entire package as gzipped tar archive"
        >
          Download package (.tar.gz)
        </a>
      </div>
      <p
        style={{
          margin: '0 0 1rem',
          fontSize: '0.78rem',
          fontFamily: 'ui-monospace, monospace',
          color: 'var(--color-text-secondary)',
          wordBreak: 'break-all',
        }}
        aria-label="Package directory"
      >
        {emittedPath}
      </p>
      <ul
        aria-label="Package artifacts"
        style={{
          listStyle: 'none',
          padding: 0,
          margin: 0,
          fontSize: '0.83rem',
        }}
      >
        {artifacts.map((a) => (
          <ArtifactRowView
            key={a.name}
            sessionId={sessionId}
            row={a}
            onOpenInline={
              a.kind === 'markdown'
                ? () => setViewer({ path: a.path, filename: a.name })
                : undefined
            }
          />
        ))}
      </ul>
      <p
        style={{
          marginTop: '1rem',
          fontSize: '0.72rem',
          color: 'var(--color-text-faint)',
          fontStyle: 'italic',
          lineHeight: 1.5,
        }}
      >
        Per-task outputs live under{' '}
        <code style={{ fontFamily: 'ui-monospace, monospace' }}>
          runtime/outputs/&lt;task_id&gt;/
        </code>{' '}
        — open the Progress tab and click any completed task to see its
        result.json, figures, and intermediate files.
      </p>
    </div>
  )
}

interface ViewerTarget {
  path: string
  filename: string
}

interface ArtifactRow {
  name: string
  description: string
  path: string
  /**
   * `markdown` rows render a "View inline" button that opens the file
   * in the shared MarkdownViewer modal. `opaque` rows (JSON, archives)
   * only get the raw link — the browser already renders JSON usably
   * and a "View inline" affordance on a non-markdown payload would
   * mislead the SME.
   */
  kind: 'markdown' | 'opaque'
}

function ArtifactRowView({
  sessionId,
  row,
  onOpenInline,
}: {
  sessionId: string
  row: ArtifactRow
  onOpenInline?: () => void
}): JSX.Element {
  const url = artifactUrl(sessionId, row.path)
  return (
    <li
      style={{
        padding: '0.5rem 0',
        borderBottom: '1px solid var(--color-border-subtle)',
        display: 'flex',
        justifyContent: 'space-between',
        gap: '1rem',
        alignItems: 'baseline',
      }}
    >
      <div style={{ display: 'flex', flexDirection: 'column', gap: '0.15rem' }}>
        <a
          href={url}
          target="_blank"
          rel="noreferrer noopener"
          style={{
            fontFamily: 'ui-monospace, monospace',
            color: 'var(--color-link, var(--color-text-primary))',
            textDecoration: 'underline',
          }}
        >
          {row.name}
        </a>
        <span
          style={{
            color: 'var(--color-text-muted)',
            fontSize: '0.76rem',
          }}
        >
          {row.description}
        </span>
      </div>
      {onOpenInline && (
        <button
          type="button"
          onClick={onOpenInline}
          style={secondaryButtonStyle}
          aria-label={`View ${row.name} inline`}
        >
          View inline
        </button>
      )}
    </li>
  )
}

function FinalReportCard({
  sessionId,
  onOpenInline,
}: {
  sessionId: string
  onOpenInline: () => void
}): JSX.Element {
  const reportPath = 'runtime/outputs/final_reporting/final_report.md'
  const url = artifactUrl(sessionId, reportPath)
  return (
    <div
      aria-label="Final report card"
      style={{
        padding: '0.85rem',
        borderRadius: '0.4rem',
        background: 'var(--color-surface-2, var(--color-surface-muted))',
        border: '1px solid var(--color-success-fg, var(--color-border-subtle))',
        borderLeft: '3px solid var(--color-success-accent, currentColor)',
      }}
    >
      <h3
        style={{
          margin: '0 0 0.4rem',
          fontSize: '0.95rem',
          color: 'var(--color-text-primary)',
        }}
      >
        Final report
      </h3>
      <p
        style={{
          margin: '0 0 0.8rem',
          fontSize: '0.8rem',
          color: 'var(--color-text-secondary)',
        }}
      >
        The narrative summary written by the final_reporting stage —
        executive summary, per-stage tables, key metrics, and figures
        embedded inline.
      </p>
      <div style={{ display: 'flex', gap: '0.5rem', flexWrap: 'wrap' }}>
        <button
          type="button"
          onClick={onOpenInline}
          style={primaryButtonStyle}
          aria-label="View final report inline"
        >
          View inline
        </button>
        <a
          href={url}
          target="_blank"
          rel="noreferrer noopener"
          style={secondaryButtonStyle}
        >
          Open raw (.md)
        </a>
        <a
          href={url}
          download="final_report.md"
          style={secondaryButtonStyle}
        >
          Download (.md)
        </a>
      </div>
    </div>
  )
}

const primaryButtonStyle: React.CSSProperties = {
  display: 'inline-block',
  padding: '0.35rem 0.75rem',
  borderRadius: '0.3rem',
  fontSize: '0.82rem',
  fontWeight: 500,
  textDecoration: 'none',
  background: 'var(--color-accent-bg, var(--color-surface-muted))',
  color: 'var(--color-accent-fg, var(--color-text-primary))',
  border: '1px solid var(--color-border-subtle)',
}

const secondaryButtonStyle: React.CSSProperties = {
  display: 'inline-block',
  padding: '0.35rem 0.75rem',
  borderRadius: '0.3rem',
  fontSize: '0.82rem',
  textDecoration: 'none',
  background: 'transparent',
  color: 'var(--color-text-primary)',
  border: '1px solid var(--color-border-subtle)',
}
