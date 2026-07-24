// Multi-analyst ensemble robustness rollup (opt-in ensemble mode).
//
// Sources: `GET …/stat-distribution` (per-entity cross-method
// robustness) and `GET …/ensemble-distribution` (per-cell
// method×lens agreement, factorial attribution, literature union).
// Both 404 (→ null) when no ensemble ran for this session's emitted
// package — that is the dominant case for ordinary (non-ensemble)
// sessions, so the tab renders a plain empty state rather than an
// error.
//
// This surface quantifies robustness/uncertainty across independently
// run analyst variants. It is explicitly NOT a truth check — running
// the same biased model N times and aggregating narrows variance, not
// bias. See the standing caveat rendered at the top of the tab.

import { useState } from 'react'
import { useCancelableEffect } from '../../hooks/useCancelableFetch'
import { getEnsembleDistribution, getStatDistribution } from '../../api/chatClient'
import { formatInteger } from '../../lib/format'
import type { EnsembleDistribution } from '../../types/EnsembleDistribution'
import type { StatDistribution } from '../../types/StatDistribution'
import type { RobustnessClass } from '../../types/RobustnessClass'
import type { EntityMethodRow } from '../../types/EntityMethodRow'
import type { CellRollup } from '../../types/CellRollup'
import type { LitFinding } from '../../types/LitFinding'

interface Props {
  sessionId: string | null
  /** Refresh on state changes so a freshly-produced ensemble appears
   *  without a manual reload. */
  refreshKey?: string | number
}

const HEADING_STYLE: React.CSSProperties = {
  fontSize: 14,
  fontWeight: 600,
  marginBottom: 4,
}

const SUBHEAD_STYLE: React.CSSProperties = {
  color: 'var(--color-text-muted, #666)',
  fontSize: 12,
  marginBottom: 12,
}

const CAVEAT_STYLE: React.CSSProperties = {
  fontSize: 12,
  fontStyle: 'italic',
  color: 'var(--color-text-secondary, #57606a)',
  background: 'var(--color-surface-1, #f6f8fa)',
  border: '1px solid var(--color-border-default, #e2e8f0)',
  borderRadius: 6,
  padding: '8px 10px',
  marginBottom: 14,
}

const SECTION_STYLE: React.CSSProperties = {
  marginBottom: 20,
}

const SECTION_HEADING_STYLE: React.CSSProperties = {
  fontSize: 13,
  fontWeight: 600,
  marginBottom: 6,
}

const TABLE_STYLE: React.CSSProperties = {
  width: '100%',
  borderCollapse: 'collapse',
  fontSize: 12,
}

const TH_STYLE: React.CSSProperties = {
  textAlign: 'left',
  padding: '5px 8px',
  borderBottom: '1px solid var(--color-border-default, #e2e8f0)',
  fontWeight: 600,
  fontSize: 11,
  color: 'var(--color-text-muted, #666)',
}

const TD_STYLE: React.CSSProperties = {
  padding: '5px 8px',
  borderBottom: '1px solid var(--color-border-subtle, #f1f5f9)',
  verticalAlign: 'top',
}

// Fixed coloring per robustness class. `RobustnessClass` is
// `#[non_exhaustive]` on the Rust side — a future variant would arrive
// over the wire as a string tsc still types as one of the four current
// members, so `CLASS_COLOR[c]` can genuinely be undefined at runtime;
// callers must fall back rather than trust the indexed lookup.
const CLASS_COLOR: Record<RobustnessClass, string> = {
  robust: '#1a7f37',
  concordant: '#0969da',
  fragile: '#9a6700',
  discordant: '#cf222e',
}

function classColor(c: string): string {
  return CLASS_COLOR[c as RobustnessClass] ?? '#57606a'
}

function classBadgeStyle(c: string): React.CSSProperties {
  const color = classColor(c)
  return {
    display: 'inline-block',
    padding: '1px 6px',
    borderRadius: 3,
    fontSize: 11,
    fontWeight: 600,
    color: '#fff',
    background: color,
  }
}

function pct(n: number): string {
  if (!Number.isFinite(n)) return '—'
  return `${(n * 100).toFixed(0)}%`
}

/**
 * Read pruned status off a `CellRollup.verification` defensively.
 * `verification` is typed `unknown | null` on the wire (it carries an
 * opaque serialized `ClaimVerificationReport`); a cell is pruned when
 * that report exists and its `n_mismatch` is truthy.
 */
function isPruned(c: CellRollup): boolean {
  return !!(
    c.verification &&
    typeof c.verification === 'object' &&
    (c.verification as { n_mismatch?: number }).n_mismatch
  )
}

export function EnsembleTab({ sessionId, refreshKey }: Props): JSX.Element {
  const [ensemble, setEnsemble] = useState<EnsembleDistribution | null>(null)
  const [stat, setStat] = useState<StatDistribution | null>(null)
  const [loaded, setLoaded] = useState(false)
  const [err, setErr] = useState<string | null>(null)

  useCancelableEffect(async ({ cancelled }) => {
    if (!sessionId) {
      setEnsemble(null)
      setStat(null)
      setLoaded(false)
      setErr(null)
      return
    }
    setLoaded(false)
    setErr(null)
    try {
      const [e, s] = await Promise.all([
        getEnsembleDistribution(sessionId),
        getStatDistribution(sessionId),
      ])
      if (!cancelled()) {
        setEnsemble(e)
        setStat(s)
        setLoaded(true)
      }
    } catch (ex) {
      if (!cancelled()) {
        setErr(ex instanceof Error ? ex.message : String(ex))
        setLoaded(true)
      }
    }
  }, [sessionId, refreshKey])

  if (!sessionId) {
    return (
      <div style={{ padding: 16, color: 'var(--color-text-muted, #666)' }}>
        No session selected.
      </div>
    )
  }

  return (
    <div style={{ padding: 16, overflowY: 'auto' }} data-testid="ensemble-tab">
      <div style={HEADING_STYLE}>Robustness / ensemble</div>
      <div style={CAVEAT_STYLE}>
        Robustness/uncertainty across analysts — not verified biological
        truth. Aggregation reduces variance, not bias.
      </div>

      {err && (
        <div style={{ fontSize: 12, color: 'var(--color-danger-fg, #cf222e)', marginBottom: 12 }}>
          Failed to load ensemble data: {err}
        </div>
      )}

      {!loaded && !err && (
        <div style={SUBHEAD_STYLE}>Loading ensemble data…</div>
      )}

      {loaded && ensemble === null && stat === null && (
        <div style={SUBHEAD_STYLE}>No ensemble was run for this package.</div>
      )}

      {loaded && stat !== null && <MethodRobustnessSection stat={stat} />}
      {loaded && ensemble !== null && <LensAgreementSection ensemble={ensemble} />}
      {loaded && ensemble !== null && (
        <InteractionHotspotsSection hotspots={ensemble.attribution.interaction_hotspots} />
      )}
      {loaded && ensemble !== null && <CellRosterSection cells={ensemble.cells} />}
      {loaded && ensemble !== null && (
        <LiteratureSection
          findings={ensemble.literature_union}
          coverage={ensemble.coverage}
        />
      )}
    </div>
  )
}

/** Class-count summary row + per-entity cross-method robustness table. */
function MethodRobustnessSection({ stat }: { stat: StatDistribution }): JSX.Element {
  return (
    <section style={SECTION_STYLE} aria-label="Method robustness">
      <div style={SECTION_HEADING_STYLE}>Method robustness ({stat.methods.length} methods)</div>
      <div style={{ marginBottom: 8, fontSize: 12 }}>
        <span style={classBadgeStyle('robust')}>Robust {formatInteger(Number(stat.n_robust))}</span>{' '}
        <span style={classBadgeStyle('concordant')}>
          Concordant {formatInteger(Number(stat.n_concordant))}
        </span>{' '}
        <span style={classBadgeStyle('fragile')}>Fragile {formatInteger(Number(stat.n_fragile))}</span>{' '}
        <span style={classBadgeStyle('discordant')}>
          Discordant {formatInteger(Number(stat.n_discordant))}
        </span>
      </div>
      {stat.entities.length === 0 ? (
        <div style={SUBHEAD_STYLE}>No entities reported by any method.</div>
      ) : (
        <table style={TABLE_STYLE}>
          <thead>
            <tr>
              <th style={TH_STYLE}>Entity</th>
              <th style={TH_STYLE}>Per-method effect</th>
              <th style={TH_STYLE}># significant</th>
              <th style={TH_STYLE}>Pooled effect (median)</th>
              <th style={TH_STYLE}>Robustness</th>
            </tr>
          </thead>
          <tbody>
            {stat.entities.map((row: EntityMethodRow) => (
              <tr key={row.entity}>
                <td style={TD_STYLE}>
                  <code>{row.entity}</code>
                </td>
                <td style={TD_STYLE}>
                  {Object.entries(row.per_method_effect)
                    .map(([m, v]) => `${m}=${v?.toFixed(2) ?? '—'}`)
                    .join(', ') || '—'}
                </td>
                <td style={TD_STYLE}>{row.n_methods_significant}</td>
                <td style={TD_STYLE}>
                  {row.pooled_effect_median !== null ? row.pooled_effect_median.toFixed(2) : '—'}
                </td>
                <td style={TD_STYLE}>
                  <span style={classBadgeStyle(row.robustness)}>{row.robustness}</span>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  )
}

/** Consensus label, ensemble-wide agreement, pruned-cell count, and the
 *  by-method / by-lens support-rate marginals. */
function LensAgreementSection({ ensemble }: { ensemble: EnsembleDistribution }): JSX.Element {
  const nPruned = Number(ensemble.n_pruned)
  return (
    <section style={SECTION_STYLE} aria-label="Lens agreement">
      <div style={SECTION_HEADING_STYLE}>Lens agreement</div>
      <div style={{ ...SUBHEAD_STYLE, marginBottom: 6 }}>{ensemble.consensus_label}</div>
      <div style={{ fontSize: 12, marginBottom: 10 }}>
        Agreement: <strong>{pct(ensemble.agreement)}</strong>
        {nPruned > 0 && (
          <span style={{ marginLeft: 10, color: 'var(--color-warning-fg, #9a6700)' }}>
            {formatInteger(nPruned)} cell{nPruned === 1 ? '' : 's'} pruned (excluded from consensus)
          </span>
        )}
      </div>
      <div style={{ display: 'flex', gap: 24, flexWrap: 'wrap' }}>
        <SupportRateTable title="By method" rates={ensemble.attribution.by_method} />
        <SupportRateTable title="By lens" rates={ensemble.attribution.by_lens} />
      </div>
    </section>
  )
}

function SupportRateTable({
  title,
  rates,
}: {
  title: string
  rates: { [key in string]?: number }
}): JSX.Element {
  const entries = Object.entries(rates)
  return (
    <div>
      <div style={{ fontSize: 12, fontWeight: 600, marginBottom: 4 }}>{title}</div>
      {entries.length === 0 ? (
        <div style={SUBHEAD_STYLE}>none</div>
      ) : (
        <table style={TABLE_STYLE}>
          <tbody>
            {entries.map(([k, v]) => (
              <tr key={k}>
                <td style={TD_STYLE}>{k}</td>
                <td style={TD_STYLE}>{v !== undefined ? pct(v) : '—'}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  )
}

function InteractionHotspotsSection({ hotspots }: { hotspots: string[] }): JSX.Element {
  return (
    <section style={SECTION_STYLE} aria-label="Interaction hotspots">
      <div style={SECTION_HEADING_STYLE}>Interaction hotspots</div>
      {hotspots.length === 0 ? (
        <div style={SUBHEAD_STYLE}>none</div>
      ) : (
        <ul style={{ margin: 0, paddingLeft: 18, fontSize: 12 }}>
          {hotspots.map((id) => (
            <li key={id}>
              <code>{id}</code>
            </li>
          ))}
        </ul>
      )}
    </section>
  )
}

/** Per-cell roster: cell id, method, lens, support verdict; pruned
 *  cells render struck-through and tagged. */
function CellRosterSection({ cells }: { cells: CellRollup[] }): JSX.Element {
  return (
    <section style={SECTION_STYLE} aria-label="Per-cell roster">
      <div style={SECTION_HEADING_STYLE}>Per-cell roster ({cells.length} cells)</div>
      {cells.length === 0 ? (
        <div style={SUBHEAD_STYLE}>none</div>
      ) : (
        <table style={TABLE_STYLE}>
          <thead>
            <tr>
              <th style={TH_STYLE}>Cell</th>
              <th style={TH_STYLE}>Method</th>
              <th style={TH_STYLE}>Lens</th>
              <th style={TH_STYLE}>Support</th>
              <th style={TH_STYLE}></th>
            </tr>
          </thead>
          <tbody>
            {cells.map((c) => {
              const pruned = isPruned(c)
              const rowStyle: React.CSSProperties = pruned
                ? { textDecoration: 'line-through', color: 'var(--color-text-muted, #666)' }
                : {}
              return (
                <tr key={c.cell_id}>
                  <td style={{ ...TD_STYLE, ...rowStyle }}>
                    <code>{c.cell_id}</code>
                  </td>
                  <td style={{ ...TD_STYLE, ...rowStyle }}>{c.method || '—'}</td>
                  <td style={{ ...TD_STYLE, ...rowStyle }}>{c.lens || '—'}</td>
                  <td style={{ ...TD_STYLE, ...rowStyle }}>
                    {c.support === null ? '—' : c.support ? 'supported' : 'not supported'}
                  </td>
                  <td style={TD_STYLE}>
                    {pruned && (
                      <span style={{ fontSize: 11, color: 'var(--color-danger-fg, #cf222e)' }}>
                        pruned — excluded from consensus
                      </span>
                    )}
                  </td>
                </tr>
              )
            })}
          </tbody>
        </table>
      )}
    </section>
  )
}

/** Deduplicated literature union across all cells plus its unique-PMID
 *  coverage count. */
function LiteratureSection({
  findings,
  coverage,
}: {
  findings: LitFinding[]
  coverage: bigint
}): JSX.Element {
  return (
    <section style={SECTION_STYLE} aria-label="Literature union">
      <div style={SECTION_HEADING_STYLE}>
        Literature union — {formatInteger(Number(coverage))} unique paper
        {Number(coverage) === 1 ? '' : 's'}
      </div>
      {findings.length === 0 ? (
        <div style={SUBHEAD_STYLE}>none</div>
      ) : (
        <ul style={{ margin: 0, paddingLeft: 18, fontSize: 12 }}>
          {findings.map((f, i) => (
            <li key={`${f.pmid}-${i}`} style={{ marginBottom: 4 }}>
              <code>{f.entity}</code>
              {' — PMID '}
              <a
                href={`https://pubmed.ncbi.nlm.nih.gov/${f.pmid}/`}
                target="_blank"
                rel="noreferrer"
                style={{ color: 'var(--color-accent, #0969da)' }}
              >
                {f.pmid}
              </a>
              {f.effect !== null && ` (effect=${f.effect.toFixed(2)})`}
              <div style={{ color: 'var(--color-text-muted, #666)', fontStyle: 'italic' }}>
                “{f.evidence_quote}”
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  )
}
