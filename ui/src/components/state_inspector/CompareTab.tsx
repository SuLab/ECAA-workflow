// Compare tab. Shows the parent package's tables and figures
// side-by-side with the current package's. Pulls from
// /cross-version-diff (the overall report) and per-table
// /cross-version-diff/tables/:name on click.

import { useMemo, useState } from 'react'
import { useCancelableEffect } from '../../hooks/useCancelableFetch'
import {
  artifactUrl,
  getCrossVersionDiff,
  getCrossVersionDiffTablePair,
  type CrossVersionTablePair,
} from '../../api/chatClient'
import { COMPARE_DIFF_THRESHOLD } from '../../lib/dashboardConstants'
import { PlaceholderPane } from './common'

interface Props {
  sessionId: string | null
}

interface Report {
  tables?: Array<{ name: string; concordance?: number }>
  figures?: Array<{ name: string; parent_path?: string; current_path?: string }>
}

export function CompareTab({ sessionId }: Props): JSX.Element {
  const [report, setReport] = useState<Report | null>(null)
  const [notFound, setNotFound] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [openTable, setOpenTable] = useState<string | null>(null)

  useCancelableEffect(async ({ cancelled }) => {
    if (!sessionId) {
      setReport(null)
      return
    }
    try {
      const body = await getCrossVersionDiff(sessionId)
      if (cancelled()) return
      if (body === null) {
        setNotFound(true)
        setError(null)
      } else {
        setReport(body as unknown as Report)
        setNotFound(false)
        setError(null)
      }
    } catch (e) {
      if (!cancelled()) setError((e as Error).message)
    }
  }, [sessionId])

  if (!sessionId) {
    return <PlaceholderPane>Start a session to compare results.</PlaceholderPane>
  }
  if (notFound) {
    return (
      <PlaceholderPane>
        This session has no parent package to compare against yet. After an
        amendment or a branch re-emit, the comparison view will appear here.
      </PlaceholderPane>
    )
  }
  if (error) {
    return <PlaceholderPane>Could not load comparison: {error}</PlaceholderPane>
  }
  if (!report) {
    return <PlaceholderPane>Loading comparison…</PlaceholderPane>
  }

  const tables = report.tables ?? []
  const figures = report.figures ?? []
  if (tables.length === 0 && figures.length === 0) {
    return (
      <PlaceholderPane>
        This package doesn't overlap with its parent on any comparable
        outputs yet.
      </PlaceholderPane>
    )
  }

  return (
    <div
      role="tabpanel"
      style={{
        flex: 1,
        minHeight: 0,
        overflowY: 'auto',
        padding: '0.9rem',
        background: 'var(--color-surface-0)',
      }}
    >
      {figures.length > 0 && (
        <>
          <h3 style={sectionTitle}>Figures</h3>
          <div
            style={{
              display: 'grid',
              gridTemplateColumns: 'repeat(auto-fill, minmax(320px, 1fr))',
              gap: '0.8rem',
              marginBottom: '1.2rem',
            }}
          >
            {figures.map((f) => (
              <FigurePair key={f.name} figure={f} sessionId={sessionId} />
            ))}
          </div>
        </>
      )}
      {tables.length > 0 && (
        <>
          <h3 style={sectionTitle}>Tables</h3>
          <ul style={{ listStyle: 'none', margin: 0, padding: 0 }}>
            {tables.map((t) => {
              const concordance =
                typeof t.concordance === 'number'
                  ? ` — ${Math.round(t.concordance * 100)}% concordant`
                  : ''
              return (
                <li key={t.name} style={{ borderTop: '1px solid var(--color-border-subtle)' }}>
                  <button
                    type="button"
                    onClick={() =>
                      setOpenTable((cur) => (cur === t.name ? null : t.name))
                    }
                    style={{
                      width: '100%',
                      textAlign: 'left',
                      padding: '0.6rem 0.4rem',
                      background: 'transparent',
                      border: 'none',
                      color: 'var(--color-text-primary)',
                      fontSize: '0.88rem',
                      cursor: 'pointer',
                    }}
                  >
                    <strong>{t.name}</strong>
                    <span style={{ color: 'var(--color-text-muted)', marginLeft: 8 }}>
                      {concordance}
                    </span>
                  </button>
                  {openTable === t.name && (
                    <TablePair sessionId={sessionId} tableName={t.name} />
                  )}
                </li>
              )
            })}
          </ul>
        </>
      )}
    </div>
  )
}

function FigurePair({
  figure,
  sessionId,
}: {
  figure: { name: string; parent_path?: string; current_path?: string }
  sessionId: string
}) {
  return (
    <div
      style={{
        border: '1px solid var(--color-border-default)',
        borderRadius: 6,
        padding: '0.5rem',
        background: 'var(--color-surface-1)',
      }}
    >
      <div
        style={{
          display: 'grid',
          gridTemplateColumns: '1fr 1fr',
          gap: '0.5rem',
        }}
      >
        <FigurePane
          sessionId={sessionId}
          path={figure.parent_path}
          label="Parent"
        />
        <FigurePane
          sessionId={sessionId}
          path={figure.current_path}
          label="Current"
        />
      </div>
      <div
        style={{
          fontSize: '0.75rem',
          textAlign: 'center',
          marginTop: 4,
          color: 'var(--color-text-secondary)',
        }}
      >
        {figure.name}
      </div>
    </div>
  )
}

function FigurePane({
  sessionId,
  path,
  label,
}: {
  sessionId: string
  path: string | undefined
  label: string
}) {
  if (!path) {
    return (
      <div
        style={{
          fontSize: '0.72rem',
          color: 'var(--color-text-muted)',
          textAlign: 'center',
          padding: '1.5rem 0',
          border: '1px dashed var(--color-border-default)',
        }}
      >
        not in {label.toLowerCase()}
      </div>
    )
  }
  return (
    <figure style={{ margin: 0 }}>
      <img
        src={artifactUrl(sessionId, path)}
        alt={`${label} ${path}`}
        style={{ width: '100%', display: 'block', borderRadius: 4 }}
      />
      <figcaption style={{ fontSize: '0.7rem', color: 'var(--color-text-muted)' }}>
        {label}
      </figcaption>
    </figure>
  )
}

function TablePair({
  sessionId,
  tableName,
}: {
  sessionId: string
  tableName: string
}) {
  const [pair, setPair] = useState<CrossVersionTablePair | null>(null)
  const [error, setError] = useState<string | null>(null)

  useCancelableEffect(async ({ cancelled }) => {
    try {
      const p = await getCrossVersionDiffTablePair(sessionId, tableName)
      if (!cancelled()) setPair(p)
    } catch (e) {
      if (!cancelled()) setError(e instanceof Error ? e.message : String(e))
    }
  }, [sessionId, tableName])

  const rows = useMemo(() => {
    if (!pair) return null
    return diffRows(pair)
  }, [pair])

  if (error) {
    return (
      <div role="alert" style={{ padding: '0.4rem', color: 'var(--color-danger-accent)' }}>
        {error}
      </div>
    )
  }
  if (!rows) {
    return (
      <div style={{ padding: '0.4rem', color: 'var(--color-text-muted)' }}>
        Loading table…
      </div>
    )
  }

  return (
    <div
      style={{
        padding: '0.4rem 0.2rem 0.8rem',
        overflowX: 'auto',
      }}
    >
      <table style={{ borderCollapse: 'collapse', fontSize: '0.78rem', width: '100%' }}>
        <thead>
          <tr>
            {rows.header.map((h, i) => (
              <th
                key={`${h}-${i}`}
                style={{
                  textAlign: 'left',
                  padding: '0.2rem 0.4rem',
                  borderBottom: '1px solid var(--color-border-default)',
                  color: 'var(--color-text-secondary)',
                }}
              >
                {h}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.body.map((r, i) => {
            const anyDiff = r.diffs?.some((d) => d) ?? false
            return (
              <tr
                key={i}
                style={{
                  background: anyDiff ? 'var(--color-warning-bg)' : 'transparent',
                }}
              >
                {r.cells.map((c, j) => (
                  <td
                    key={j}
                    style={{
                      padding: '0.2rem 0.4rem',
                      borderBottom: '1px solid var(--color-border-subtle)',
                      fontFamily: 'ui-monospace, monospace',
                    }}
                  >
                    {c}
                  </td>
                ))}
              </tr>
            )
          })}
        </tbody>
      </table>
    </div>
  )
}

// Quote-aware CSV/TSV split. The previous hand-rolled `line.split(sep)`
// mangled rows whose cells embedded the separator inside double-quoted
// fields (common in narrative columns + gene-symbol lists). We:
//  - split rows by any newline outside a quoted region
//  - split cells by the separator outside a quoted region using the
//    even-quotes-ahead lookahead trick
//  - strip surrounding double quotes
//  - unescape doubled quotes (the RFC 4180 escape mechanism)
// We don't pull in Papa Parse: it would double the bundle for one
// 30-line helper. TSVs don't have the same quoting tradition, so the
// quote handling there is a no-op in practice.
function parseDelim(body: string, mime: string): string[][] {
  const isTsv = mime.includes('tab-separated')
  const sep = isTsv ? '\t' : ','
  const cellSplit = isTsv
    ? /\t/
    : /,(?=(?:(?:[^"]*"){2})*[^"]*$)/
  return splitLinesOutsideQuotes(body)
    .filter((line) => line.length > 0)
    .map((line) =>
      line
        .split(cellSplit)
        .map((cell) => unquoteCsvCell(cell, sep === ',')),
    )
}

/**
 * Split on \r?\n that sits outside any unterminated double-quoted
 * region. The body is walked once tracking an `in_quote` toggle.
 */
function splitLinesOutsideQuotes(body: string): string[] {
  const out: string[] = []
  let cur = ''
  let inQuote = false
  for (let i = 0; i < body.length; i++) {
    const ch = body[i]
    if (ch === '"') {
      // RFC 4180 doubled-quote escape: `""` is a literal quote and does
      // not toggle the `inQuote` state.
      if (inQuote && body[i + 1] === '"') {
        cur += '""'
        i++
        continue
      }
      inQuote = !inQuote
      cur += ch
      continue
    }
    if (!inQuote && (ch === '\n' || ch === '\r')) {
      // Swallow paired \r\n.
      if (ch === '\r' && body[i + 1] === '\n') i++
      out.push(cur)
      cur = ''
      continue
    }
    cur += ch
  }
  if (cur.length > 0) out.push(cur)
  return out
}

/**
 * Strip RFC-4180 quoting from a cell: leading/trailing `"` and doubled
 * inner `""` collapsed to a single `"`. Cells from TSV input are
 * returned untouched (`isCsv=false`) since the regex split there
 * doesn't recognise quote-protected separators.
 */
function unquoteCsvCell(cell: string, isCsv: boolean): string {
  if (!isCsv) return cell
  if (cell.length >= 2 && cell.startsWith('"') && cell.endsWith('"')) {
    return cell.slice(1, -1).replace(/""/g, '"')
  }
  return cell
}

function diffRows(pair: CrossVersionTablePair): {
  header: string[]
  body: Array<{ cells: string[]; diffs?: boolean[] }>
} {
  const current = pair.current
    ? parseDelim(pair.current.body, pair.current.mime)
    : []
  const parent = pair.parent
    ? parseDelim(pair.parent.body, pair.parent.mime)
    : []
  const headerA = current[0] ?? []
  const headerB = parent[0] ?? []
  // Render the union of columns: [header columns], with a parent/current
  // split annotation when both exist.
  const header = headerA.length >= headerB.length ? headerA : headerB
  const dataA = current.slice(1)
  const dataB = parent.slice(1)
  const n = Math.max(dataA.length, dataB.length)
  const body: Array<{ cells: string[]; diffs?: boolean[] }> = []
  for (let i = 0; i < n; i++) {
    const a = dataA[i] ?? []
    const b = dataB[i] ?? []
    const cells: string[] = []
    const diffs: boolean[] = []
    for (let c = 0; c < header.length; c++) {
      const av = a[c] ?? ''
      const bv = b[c] ?? ''
      const combined = bv === av || bv === ''
        ? av || bv
        : `${bv} → ${av}`
      cells.push(combined)
      diffs.push(numericallyDiffers(av, bv))
    }
    body.push({ cells, diffs })
  }
  return { header, body }
}

function numericallyDiffers(a: string, b: string): boolean {
  const fa = parseFloat(a)
  const fb = parseFloat(b)
  if (!Number.isFinite(fa) || !Number.isFinite(fb)) return a !== b
  const denom = Math.max(Math.abs(fa), Math.abs(fb))
  if (denom === 0) return false
  return Math.abs(fa - fb) / denom > COMPARE_DIFF_THRESHOLD
}

const sectionTitle: React.CSSProperties = {
  fontSize: '0.78rem',
  textTransform: 'uppercase',
  letterSpacing: '0.05em',
  color: 'var(--color-text-secondary)',
  margin: '0 0 0.5rem',
}
