// Inline turn-card for sensitivity-comparison stages (class:
// sensitivity_comparison). Rendered when the session surfaces
// BlockerKind::AwaitingSmeSelection. Exposes a radio group of candidate
// variants plus an optional rationale textarea. On confirm, dispatches
// into the select_sensitivity_winner tool via onSelect.
//
// When a `crossVersion` report is supplied, the card switches to a
// read-only per-table concordance view that compares a parent package
// vs a child package (no radio group, no submit).

import { useState } from 'react'
import type { CrossVersionReport } from '../types'
import { useAsync } from '../hooks/useAsync'
import { stageIdToLabel } from '../lib/stageLabels'
import { RadioRow, type RadioOption } from './primitives/RadioRow'

interface Props {
  stage: string
  candidates: string[]
  onSelect: (winner: string, rationale?: string) => void | Promise<void>
  disabled?: boolean
  /**
   * When non-null, renders the cross-version concordance view instead
   * of the methods A/B radio group. Callers that don't pass this prop
   * (or pass null/undefined) get the original behavior.
   */
  crossVersion?: CrossVersionReport | null
}

export default function SensitivityComparisonCard({
  stage,
  candidates,
  onSelect,
  disabled,
  crossVersion,
}: Props) {
  const [winner, setWinner] = useState<string | null>(null)
  const [rationale, setRationale] = useState('')
  const { busy: submitting, run } = useAsync()

  const mode: 'methods' | 'versions' =
    crossVersion != null ? 'versions' : 'methods'

  // Sanitize stage to a valid HTML id. Stage ids are server-supplied so
  // could in theory carry whitespace or special chars; aria-labelledby
  // needs a clean id reference.
  const headingId = `sensitivity-${stage.replace(/[^a-zA-Z0-9_-]/g, '_')}-heading`
  const canSubmit = winner !== null && !disabled && !submitting

  const handleSubmit = async () => {
    if (!winner) return
    await run(() =>
      Promise.resolve(
        onSelect(winner, rationale.trim() ? rationale.trim() : undefined),
      ),
    )
  }

  return (
    <section
      role="region"
      aria-labelledby={headingId}
      data-stage-id={stage}
      data-mode={mode}
      style={{
        marginTop: '0.75rem',
        padding: '0.85rem 1rem',
        background: 'var(--color-info-bg)',
        border: '1px solid #93c5fd',
        borderLeft: '4px solid #2563eb',
        borderRadius: 8,
      }}
    >
      <h3
        id={headingId}
        style={{
          margin: 0,
          marginBottom: 8,
          fontSize: '0.9rem',
          color: 'var(--color-info-fg)',
        }}
      >
        {mode === 'versions' ? (
          <>
            Cross-version diff for <span title={stage}>{stageIdToLabel(stage)}</span>
          </>
        ) : (
          <>
            Pick a winner for <span title={stage}>{stageIdToLabel(stage)}</span>
          </>
        )}
      </h3>

      {mode === 'versions' && crossVersion ? (
        <CrossVersionView report={crossVersion} />
      ) : (
        <MethodsPicker
          stage={stage}
          candidates={candidates}
          headingId={headingId}
          disabled={disabled}
          winner={winner}
          setWinner={setWinner}
          rationale={rationale}
          setRationale={setRationale}
          canSubmit={canSubmit}
          onSubmit={handleSubmit}
        />
      )}
    </section>
  )
}

interface MethodsPickerProps {
  stage: string
  candidates: string[]
  headingId: string
  disabled: boolean | undefined
  winner: string | null
  setWinner: (w: string) => void
  rationale: string
  setRationale: (r: string) => void
  canSubmit: boolean
  onSubmit: () => void | Promise<void>
}

function MethodsPicker({
  stage,
  candidates,
  headingId: _headingId,
  disabled,
  winner,
  setWinner,
  rationale,
  setRationale,
  canSubmit,
  onSubmit,
}: MethodsPickerProps) {
  return (
    <>
      {candidates.length === 0 ? (
        <p
          style={{
            margin: 0,
            fontSize: '0.82rem',
            color: 'var(--color-info-fg)',
            fontStyle: 'italic',
          }}
        >
          No candidates yet — results haven't arrived.
        </p>
      ) : (
        <div style={{ marginBottom: '0.6rem' }}>
          <RadioRow<string>
            name={`sensitivity-${stage}`}
            ariaLabel={`Sensitivity winner for ${stage}`}
            value={winner}
            onChange={setWinner}
            options={candidates.map<RadioOption<string>>((c) => ({
              value: c,
              ariaLabel: `Select ${c}`,
              label: <span>{c}</span>,
              disabled,
            }))}
          />
        </div>
      )}

      <label
        style={{
          display: 'block',
          fontSize: '0.78rem',
          color: 'var(--color-info-fg)',
          marginBottom: 4,
        }}
      >
        Optional rationale
      </label>
      <textarea
        value={rationale}
        onChange={(e) => setRationale(e.target.value)}
        disabled={disabled}
        aria-label="Optional rationale"
        rows={2}
        placeholder="e.g. tighter silhouette at the cell-type boundaries"
        style={{
          width: '100%',
          boxSizing: 'border-box',
          padding: '0.4rem 0.55rem',
          fontSize: '0.8rem',
          fontFamily: 'inherit',
          border: '1px solid #bfdbfe',
          borderRadius: 6,
          resize: 'vertical',
          marginBottom: '0.6rem',
        }}
      />

      <button
        type="button"
        onClick={onSubmit}
        disabled={!canSubmit}
        style={{
          padding: '0.45rem 0.9rem',
          background: canSubmit ? 'var(--color-accent)' : 'var(--color-info-border)',
          color: 'var(--color-text-on-accent)',
          border: 'none',
          borderRadius: 6,
          cursor: canSubmit ? 'pointer' : 'not-allowed',
          fontSize: '0.8rem',
          fontWeight: 600,
        }}
      >
        Record choice
      </button>
    </>
  )
}

function CrossVersionView({ report }: { report: CrossVersionReport }) {
  return (
    <div>
      <p
        style={{
          margin: '0 0 0.6rem',
          fontSize: '0.82rem',
          color: 'var(--color-info-fg)',
        }}
      >
        Comparing{' '}
        <strong>
          <code style={{ fontFamily: 'ui-monospace, monospace' }}>
            {report.parent_package}
          </code>
        </strong>{' '}
        (version A) vs{' '}
        <strong>
          <code style={{ fontFamily: 'ui-monospace, monospace' }}>
            {report.child_package}
          </code>
        </strong>{' '}
        (version B). Overall concordance:{' '}
        <strong>{(report.overall_concordance * 100).toFixed(1)}%</strong>
      </p>

      {report.tables.length === 0 ? (
        <p
          style={{
            margin: 0,
            fontSize: '0.82rem',
            color: 'var(--color-info-fg)',
            fontStyle: 'italic',
          }}
        >
          No tables to compare — the diff is empty.
        </p>
      ) : (
        <ul
          aria-label="Per-table concordance"
          style={{
            listStyle: 'none',
            margin: 0,
            padding: 0,
            display: 'flex',
            flexDirection: 'column',
            gap: '0.45rem',
          }}
        >
          {report.tables.map((t) => {
            const total = t.n_robust + t.n_concordant + t.n_discordant
            const pctRobust = total > 0 ? (t.n_robust / total) * 100 : 0
            const pctConcordant = total > 0 ? (t.n_concordant / total) * 100 : 0
            const pctDiscordant = total > 0 ? (t.n_discordant / total) * 100 : 0
            return (
              <li
                key={t.table_name}
                data-table-name={t.table_name}
                style={{
                  padding: '0.45rem 0.6rem',
                  background: 'var(--color-surface-1)',
                  border: '1px solid #bfdbfe',
                  borderRadius: 6,
                }}
              >
                <div
                  style={{
                    display: 'flex',
                    justifyContent: 'space-between',
                    fontSize: '0.82rem',
                    color: 'var(--color-info-fg)',
                    marginBottom: 4,
                  }}
                >
                  <span
                    style={{
                      fontFamily: 'ui-monospace, monospace',
                      fontWeight: 600,
                    }}
                  >
                    {t.table_name}
                  </span>
                  <span style={{ color: 'var(--color-text-primary)' }}>
                    robust {t.n_robust} · concordant {t.n_concordant} ·
                    discordant {t.n_discordant}
                  </span>
                </div>
                <div
                  role="progressbar"
                  aria-label={`${t.table_name} concordance`}
                  aria-valuenow={Math.round(pctRobust + pctConcordant)}
                  aria-valuemin={0}
                  aria-valuemax={100}
                  style={{
                    display: 'flex',
                    width: '100%',
                    height: 8,
                    borderRadius: 4,
                    overflow: 'hidden',
                    background: 'var(--color-border-default)',
                  }}
                >
                  <div
                    data-segment="robust"
                    style={{
                      width: `${pctRobust}%`,
                      background: 'var(--color-success-accent)',
                    }}
                  />
                  <div
                    data-segment="concordant"
                    style={{
                      width: `${pctConcordant}%`,
                      background: 'var(--color-warning-accent)',
                    }}
                  />
                  <div
                    data-segment="discordant"
                    style={{
                      width: `${pctDiscordant}%`,
                      background: 'var(--color-danger-accent)',
                    }}
                  />
                </div>
              </li>
            )
          })}
        </ul>
      )}
    </div>
  )
}
