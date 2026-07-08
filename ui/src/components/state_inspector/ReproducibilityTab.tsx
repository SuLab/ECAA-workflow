// Reproducibility surface.
//
// Two deterministic, server-side attestations over the emitted package
// (no LLM Tool involved):
//
//   1. Audit-proof re-verify — re-runs the 6 audit-proof invariants
//      (claim_completeness / decision_justification / evidence_coverage /
//      equivalence_failure / cross_graph_integrity / substrate_validity)
//      with the session's in-process HMAC secret, so claim-completeness
//      is no longer vacuous. Renders one row per invariant with
//      status + inspected/violation counts and a Re-verify button.
//
//   2. Replay re-verifier — Tier-1 "integrity check" is a synchronous
//      offline tamper/drift verdict; Tier-2 "full reproduce" re-runs the
//      recorded compute in containers (backgrounded; POST returns 202,
//      polled every 3s). A missing container runtime surfaces as a
//      PARTIAL verdict via `reexecute.unprovisionable`.
//
// The component self-fetches on mount (mirrors ClaimsTab) and polls the
// backgrounded replay job independently of the SSE stream — the
// replay_started / replay_completed SSE events are advisory here.

import { useCallback, useEffect, useState } from 'react'
import type { AuditProofReport } from '../../types/AuditProofReport'
import type { InvariantStatus } from '../../types/InvariantStatus'
import type { PackageCapabilities } from '../../types/PackageCapabilities'
import type { ReplayReport } from '../../types/ReplayReport'
import type { ReplayVerdict } from '../../types/ReplayVerdict'
import {
  getAuditProof,
  getReplay,
  replayVerify,
  reverifyAuditProof,
  startReplayReproduce,
} from '../../api/chatClient'

interface Props {
  sessionId: string | null
  /// True when the session was reconstructed from an uploaded package.
  /// The originating HMAC signing secret never leaves its process, so
  /// re-verification is structural (verifier-less) — surfaced via the
  /// note near the Re-verify control.
  imported?: boolean
  /// Physical-presence capability probe. Used to disable Tier-2 full
  /// reproduce when the package is not re-executable
  /// (`replay_tier2 === false`).
  capabilities?: PackageCapabilities
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

const SECTION_STYLE: React.CSSProperties = {
  marginTop: 20,
  paddingTop: 16,
  borderTop: '1px solid var(--color-border-subtle, #f1f5f9)',
}

const TABLE_STYLE: React.CSSProperties = {
  width: '100%',
  borderCollapse: 'collapse',
  fontSize: 13,
}

const TH_STYLE: React.CSSProperties = {
  textAlign: 'left',
  padding: '6px 8px',
  borderBottom: '1px solid var(--color-border-default, #e2e8f0)',
  fontWeight: 600,
  fontSize: 12,
  color: 'var(--color-text-muted, #666)',
}

const TD_STYLE: React.CSSProperties = {
  padding: '6px 8px',
  borderBottom: '1px solid var(--color-border-subtle, #f1f5f9)',
  verticalAlign: 'top',
}

const BUTTON_STYLE: React.CSSProperties = {
  fontSize: 12,
  padding: '4px 12px',
  marginTop: 10,
  cursor: 'pointer',
}

const PILL_BASE: React.CSSProperties = {
  display: 'inline-block',
  padding: '1px 6px',
  borderRadius: 3,
  fontSize: 11,
  fontWeight: 600,
  textTransform: 'uppercase',
}

function invariantPillStyle(status: InvariantStatus): React.CSSProperties {
  switch (status) {
    case 'pass':
      return { ...PILL_BASE, background: '#dcfce7', color: '#166534' }
    case 'fail':
      return { ...PILL_BASE, background: '#fef2f2', color: '#991b1b' }
    case 'warn':
      return { ...PILL_BASE, background: '#fef3c7', color: '#92400e' }
    case 'unverified':
      return {
        ...PILL_BASE,
        background: 'var(--color-surface-2, #f1f5f9)',
        color: 'var(--color-text-muted, #666)',
      }
    default:
      return {
        ...PILL_BASE,
        background: 'var(--color-surface-2, #f1f5f9)',
        color: 'var(--color-text-muted, #666)',
      }
  }
}

function verdictPillStyle(verdict: ReplayVerdict): React.CSSProperties {
  switch (verdict) {
    case 'pass':
      return { ...PILL_BASE, background: '#dcfce7', color: '#166534' }
    case 'fail':
      return { ...PILL_BASE, background: '#fef2f2', color: '#991b1b' }
    case 'partial':
      return { ...PILL_BASE, background: '#fef3c7', color: '#92400e' }
    default:
      return {
        ...PILL_BASE,
        background: 'var(--color-surface-2, #f1f5f9)',
        color: 'var(--color-text-muted, #666)',
      }
  }
}

export function ReproducibilityTab({
  sessionId,
  imported,
  capabilities,
}: Props): JSX.Element {
  const [report, setReport] = useState<AuditProofReport | null>(null)
  const [busy, setBusy] = useState(false)
  const [integrity, setIntegrity] = useState<ReplayReport | null>(null)
  const [integrityRunning, setIntegrityRunning] = useState(false)
  const [err, setErr] = useState<string | null>(null)

  // Full-reproduce (Tier-2) job state. `reproStatus` is the server's
  // backgrounded job status (`idle | running | done | failed`).
  const [reproStatus, setReproStatus] = useState<string>('idle')
  const [reproReport, setReproReport] = useState<ReplayReport | null>(null)

  // Load the last-written audit-proof report on mount / session change.
  useEffect(() => {
    if (!sessionId) {
      setReport(null)
      return
    }
    let cancelled = false
    void getAuditProof(sessionId)
      .then((r) => {
        if (!cancelled) setReport(r)
      })
      .catch(() => {
        if (!cancelled) setReport(null)
      })
    // Rehydrate a Tier-2 replay job that is still running (or already finished)
    // server-side, so switching away from and back to the tab doesn't lose it.
    void getReplay(sessionId)
      .then((s) => {
        if (cancelled) return
        if (s.status === 'running') {
          setReproStatus('running')
        } else if (s.status === 'done' || s.status === 'failed') {
          setReproStatus(s.status)
          setReproReport(s.report ?? null)
          if (s.status === 'failed' && s.error) setErr(s.error)
        }
      })
      .catch(() => {
        /* no replay job for this session yet — leave idle */
      })
    return () => {
      cancelled = true
    }
  }, [sessionId])

  const onReverify = useCallback(async () => {
    if (!sessionId) return
    setBusy(true)
    setErr(null)
    try {
      setReport(await reverifyAuditProof(sessionId))
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }, [sessionId])

  const onIntegrity = useCallback(async () => {
    if (!sessionId) return
    setIntegrityRunning(true)
    setErr(null)
    try {
      setIntegrity(await replayVerify(sessionId))
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e))
    } finally {
      setIntegrityRunning(false)
    }
  }, [sessionId])

  const onReproduce = useCallback(async () => {
    if (!sessionId) return
    // A Tier-2 replay of an IMPORTED (untrusted) package pulls its container
    // image and re-executes the code recorded inside it. Gate that behind an
    // explicit SME confirmation; locally-authored packages are unaffected.
    if (imported) {
      const ok = window.confirm(
        'This will execute code recorded in the uploaded package inside a sandbox, including pulling its container image. Continue?',
      )
      if (!ok) return
    }
    setErr(null)
    setReproReport(null)
    setReproStatus('running')
    try {
      // Tier-2 returns 202 { replay_id }; the terminal report arrives via the
      // poll below (SSE replay_completed is advisory).
      await startReplayReproduce(
        sessionId,
        imported ? { confirmed: true } : {},
      )
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e))
      setReproStatus('idle')
    }
  }, [sessionId, imported])

  // Poll the backgrounded replay job while it's running.
  useEffect(() => {
    if (!sessionId || reproStatus !== 'running') return
    let cancelled = false
    const t = setInterval(() => {
      void (async () => {
        try {
          const s = await getReplay(sessionId)
          if (cancelled) return
          if (s.status !== 'running') {
            setReproStatus(s.status)
            setReproReport(s.report ?? null)
            if (s.status === 'failed' && s.error) setErr(s.error)
          }
        } catch {
          // Transient GET failure — keep polling; the server-side job is
          // unaffected. Only an explicit status:'failed' payload (handled
          // above) surfaces an error, so one network blip won't abandon a
          // live replay.
          if (cancelled) return
        }
      })()
    }, 3000)
    return () => {
      cancelled = true
      clearInterval(t)
    }
  }, [sessionId, reproStatus])

  if (!sessionId) {
    return (
      <div style={{ padding: 16, color: 'var(--color-text-muted, #666)' }}>
        No session selected.
      </div>
    )
  }

  const reproDone = reproStatus !== 'idle' && reproStatus !== 'running'
  const reproVerdict = reproReport?.verdict ?? null

  return (
    <div style={{ padding: 16, overflowY: 'auto' }} data-testid="reproducibility-tab">
      <div style={HEADING_STYLE}>Audit-proof invariants</div>
      <div style={SUBHEAD_STYLE}>
        Re-runs the six audit-proof invariants with the session's signing
        secret so claim-completeness is attested rather than vacuous.
      </div>

      {!report ? (
        <div style={{ fontSize: 13, color: 'var(--color-text-muted, #666)' }}>
          No audit-proof report yet — emit and execute a package first.
        </div>
      ) : (
        <table style={TABLE_STYLE}>
          <thead>
            <tr>
              <th style={TH_STYLE}>Invariant</th>
              <th style={TH_STYLE}>Status</th>
              <th style={TH_STYLE}>Inspected</th>
              <th style={TH_STYLE}>Violations</th>
            </tr>
          </thead>
          <tbody>
            {report.verdicts.map((v) => (
              <tr key={v.id} data-testid={`invariant-row-${v.id}`}>
                <td style={TD_STYLE}>
                  <code>{v.id}</code>
                  {v.detail ? (
                    <div
                      style={{
                        fontSize: 11,
                        color: 'var(--color-text-muted, #666)',
                        marginTop: 2,
                      }}
                    >
                      {v.detail}
                    </div>
                  ) : null}
                </td>
                <td style={TD_STYLE}>
                  <span style={invariantPillStyle(v.status)}>{v.status}</span>
                </td>
                <td style={TD_STYLE}>{v.n_inspected}</td>
                <td style={TD_STYLE}>{v.n_violations}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      <button
        type="button"
        onClick={() => void onReverify()}
        disabled={busy}
        style={{ ...BUTTON_STYLE, cursor: busy ? 'wait' : 'pointer' }}
        data-testid="reverify-button"
      >
        {busy ? 'Re-verifying…' : 'Re-verify'}
      </button>

      {imported ? (
        <p
          data-testid="verifierless-note"
          style={{
            fontSize: 12,
            marginTop: 8,
            color: 'var(--color-text-muted, #666)',
          }}
        >
          Structural re-verification only — the originating signing secret is
          not available for uploaded packages, so claim-completeness is
          attested against the package's own recorded provenance rather than
          the origin signature.
        </p>
      ) : null}

      <div style={SECTION_STYLE}>
        <div style={HEADING_STYLE}>Replay — integrity check (offline)</div>
        <div style={SUBHEAD_STYLE}>
          Deterministic tamper / drift check against the package's recorded
          provenance. No compute is re-run.
        </div>
        <button
          type="button"
          onClick={() => void onIntegrity()}
          disabled={integrityRunning}
          style={{
            ...BUTTON_STYLE,
            marginTop: 0,
            cursor: integrityRunning ? 'wait' : 'pointer',
          }}
          data-testid="integrity-button"
        >
          {integrityRunning ? 'Checking…' : 'Run integrity check'}
        </button>
        {integrity ? (
          <p style={{ fontSize: 13, marginTop: 10 }} data-testid="integrity-verdict">
            Verdict: <span style={verdictPillStyle(integrity.verdict)}>{integrity.verdict}</span>
          </p>
        ) : null}
      </div>

      <div style={SECTION_STYLE}>
        <div style={HEADING_STYLE}>Replay — full reproduce</div>
        <div style={SUBHEAD_STYLE}>
          Re-runs the recorded compute in containers and compares outputs.
          Requires a container runtime (Docker / Podman).
        </div>
        <button
          type="button"
          onClick={() => void onReproduce()}
          disabled={reproStatus === 'running' || capabilities?.replay_tier2 === false}
          title={
            capabilities?.replay_tier2 === false
              ? 'Requires a re-executable package (scripts + result tables + a provisionable environment). This package is not re-executable.'
              : undefined
          }
          style={{
            ...BUTTON_STYLE,
            marginTop: 0,
            cursor:
              reproStatus === 'running'
                ? 'wait'
                : capabilities?.replay_tier2 === false
                  ? 'not-allowed'
                  : 'pointer',
          }}
          data-testid="reproduce-button"
        >
          {reproStatus === 'running' ? 'Reproducing…' : 'Run full reproduce'}
        </button>
        {reproDone ? (
          <p style={{ fontSize: 13, marginTop: 10 }} data-testid="reproduce-result">
            Result:{' '}
            {reproVerdict ? (
              <span style={verdictPillStyle(reproVerdict)}>{reproVerdict}</span>
            ) : (
              <strong>{reproStatus}</strong>
            )}
            {reproReport?.reexecute?.unprovisionable ? (
              <span
                style={{
                  display: 'block',
                  marginTop: 6,
                  color: '#92400e',
                  fontSize: 12,
                }}
                data-testid="unprovisionable-explainer"
              >
                PARTIAL — no container runtime available. Install Docker or
                Podman to reproduce the recorded compute; the integrity check
                above still ran offline.
              </span>
            ) : null}
          </p>
        ) : null}
      </div>

      {err ? (
        <p role="alert" style={{ color: '#991b1b', fontSize: 12, marginTop: 12 }}>
          {err}
        </p>
      ) : null}
    </div>
  )
}
