import { useState } from 'react'
import { useCancelableEffect } from '../hooks/useCancelableFetch'
import { formatUSD } from '../lib/format'
import {
  applyRemediation,
  getRemediationSuggestions,
  type ApplyRemediationResponse,
  type RemediationSuggestionsResponse,
} from '../api/chatClient'
import type { RemediationKind, RemediationSuggestion, ToolErrorEnvelope } from '../types'

interface Props {
  sessionId: string
  taskId: string
  /** Called after a remediation has been successfully applied so the
   *  parent BlockerCard can refresh state, dismiss the card, etc. */
  onApplied?: (result: ApplyRemediationResponse) => void
}

/**
 * BlockerCard variant for `BlockerKind::ToolError`. Lists ranked
 * remediation suggestions returned by the proposer, lets the SME
 * apply one, and surfaces the audit trail (attempts consumed,
 * cost delta, evidence chips). Calls
 * `/task/:task_id/remediation-suggestions` on mount and re-fetches
 * after an apply (next attempt produces a fresh envelope, so the
 * cache key changes and a new proposal set arrives).
 */
export default function RemediationSuggestionList({ sessionId, taskId, onApplied }: Props) {
  const [data, setData] = useState<RemediationSuggestionsResponse | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [applyingId, setApplyingId] = useState<string | null>(null)
  const [rationale, setRationale] = useState('')
  const [appliedNote, setAppliedNote] = useState<string | null>(null)

  useCancelableEffect(async ({ cancelled }) => {
    setLoading(true)
    setError(null)
    try {
      const r = await getRemediationSuggestions(sessionId, taskId)
      if (cancelled()) return
      if (r === null) {
        setError('No structured error envelope for this task — task may not have failed yet.')
      } else {
        setData(r)
      }
    } catch (e) {
      if (!cancelled()) {
        setError(e instanceof Error ? e.message : String(e))
      }
    } finally {
      if (!cancelled()) setLoading(false)
    }
  }, [sessionId, taskId])

  async function handleApply(s: RemediationSuggestion) {
    setApplyingId(s.id)
    setError(null)
    setAppliedNote(null)
    try {
      const result = await applyRemediation(sessionId, taskId, s.id, rationale || undefined)
      setAppliedNote(result.message)
      onApplied?.(result)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setApplyingId(null)
    }
  }

  if (loading) {
    return (
      <div style={{ padding: 12, color: '#666', fontSize: 13 }}>
        Loading remediation suggestions…
      </div>
    )
  }
  if (error) {
    return (
      <div style={{ padding: 12, color: '#a33', fontSize: 13 }}>
        {error}
      </div>
    )
  }
  if (!data) {
    return null
  }

  return (
    <div data-testid="remediation-suggestion-list" style={{ padding: 12 }}>
      <ErrorEnvelopeChips envelope={data.envelope} attempts={data.attempts_consumed} />
      {data.suggestions.length === 0 ? (
        <div style={{ color: '#666', fontSize: 13, marginTop: 12 }}>
          The proposer returned no actionable suggestions. Manual review required.
        </div>
      ) : (
        <ol
          style={{
            marginTop: 12,
            paddingLeft: 0,
            listStyle: 'none',
            display: 'flex',
            flexDirection: 'column',
            gap: 12,
          }}
        >
          {data.suggestions.map((s) => (
            <li
              key={s.id}
              style={{
                border: '1px solid #ccc',
                borderRadius: 6,
                padding: 12,
                background: '#fafafa',
              }}
            >
              <SuggestionHeader suggestion={s} />
              <div style={{ marginTop: 8, fontSize: 13, color: '#333' }}>{s.rationale}</div>
              {s.evidence.length > 0 && (
                <div style={{ marginTop: 6, fontSize: 12, color: '#666' }}>
                  Evidence:{' '}
                  {s.evidence.map((e) => (
                    <span
                      key={e}
                      style={{
                        marginRight: 6,
                        background: '#eee',
                        padding: '1px 6px',
                        borderRadius: 3,
                        fontFamily: 'monospace',
                      }}
                    >
                      {e}
                    </span>
                  ))}
                </div>
              )}
              <KindDetail kind={s.kind} />
              <button
                type="button"
                disabled={applyingId !== null}
                onClick={() => handleApply(s)}
                style={{
                  marginTop: 10,
                  padding: '6px 14px',
                  background: applyingId === s.id ? '#999' : '#2962ff',
                  color: 'white',
                  border: 'none',
                  borderRadius: 4,
                  cursor: applyingId === null ? 'pointer' : 'wait',
                  fontSize: 13,
                  fontWeight: 600,
                }}
              >
                {applyingId === s.id ? 'Applying…' : applyButtonLabel(s)}
              </button>
            </li>
          ))}
        </ol>
      )}
      <div style={{ marginTop: 12 }}>
        <label style={{ fontSize: 12, color: '#666', display: 'block' }}>
          Rationale (optional, recorded in the audit trail)
        </label>
        <input
          type="text"
          value={rationale}
          onChange={(e) => setRationale(e.target.value)}
          placeholder="e.g. retry with 64GB on the spot interrupt"
          style={{
            width: '100%',
            padding: '4px 8px',
            fontSize: 13,
            border: '1px solid #ccc',
            borderRadius: 3,
          }}
        />
      </div>
      {appliedNote && (
        <div style={{ marginTop: 10, padding: 8, background: '#e8f5e9', color: '#2e7d32', borderRadius: 4, fontSize: 13 }}>
          {appliedNote}
        </div>
      )}
    </div>
  )
}

function ErrorEnvelopeChips({ envelope, attempts }: { envelope: ToolErrorEnvelope; attempts: number }) {
  const chips: Array<{ label: string; value: string }> = []
  chips.push({ label: 'class', value: envelope.error_class })
  if (envelope.library) chips.push({ label: 'lib', value: envelope.library })
  if (envelope.signal) chips.push({ label: 'signal', value: envelope.signal })
  if (envelope.exit_code !== undefined && envelope.exit_code !== null) {
    chips.push({ label: 'exit', value: String(envelope.exit_code) })
  }
  if (envelope.peak_memory_mb) chips.push({ label: 'peak RSS', value: `${envelope.peak_memory_mb} MiB` })
  if (envelope.wallclock_secs) chips.push({ label: 'wallclock', value: `${envelope.wallclock_secs}s` })
  chips.push({ label: 'executor', value: envelope.executor })
  chips.push({ label: 'attempt', value: `${envelope.attempt}/5` })
  if (attempts !== envelope.attempt) {
    chips.push({ label: 'applied', value: String(attempts) })
  }
  return (
    <div style={{ display: 'flex', flexWrap: 'wrap', gap: 6 }}>
      {chips.map((c) => (
        <span
          key={c.label + c.value}
          style={{
            background: '#fff3e0',
            color: '#bf360c',
            padding: '2px 8px',
            borderRadius: 3,
            fontFamily: 'monospace',
            fontSize: 12,
          }}
        >
          <span style={{ color: '#7a4a00', marginRight: 4 }}>{c.label}:</span>
          {c.value}
        </span>
      ))}
    </div>
  )
}

function SuggestionHeader({ suggestion }: { suggestion: RemediationSuggestion }) {
  const conf = suggestion.confidence
  const confColor = conf === 'high' ? '#2e7d32' : conf === 'medium' ? '#ef6c00' : '#888'
  return (
    <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'baseline' }}>
      <div style={{ fontWeight: 600, fontSize: 14 }}>{kindHeading(suggestion.kind)}</div>
      <div style={{ fontSize: 12 }}>
        <span style={{ color: confColor, marginRight: 8, textTransform: 'uppercase' }}>{conf}</span>
        {suggestion.estimated_cost_delta_usd !== undefined && suggestion.estimated_cost_delta_usd !== null && (
          <span style={{ color: '#666' }}>
            +{formatUSD(suggestion.estimated_cost_delta_usd)}/run
          </span>
        )}
      </div>
    </div>
  )
}

function KindDetail({ kind }: { kind: RemediationKind }) {
  switch (kind.kind) {
    case 'bump_resources': {
      const t = kind.target
      const parts: string[] = []
      if (t.memory_gb) parts.push(`${t.memory_gb} GiB`)
      if (t.vcpus) parts.push(`${t.vcpus} vCPU`)
      if (t.storage_gb) parts.push(`${t.storage_gb} GiB disk`)
      if (t.wallclock_secs) parts.push(`${t.wallclock_secs}s wallclock`)
      if (t.gpu) parts.push(`${t.gpu.count}× ${t.gpu.kind}`)
      return <KvLine label="bump to" value={parts.join(', ') || '(no target set)'} />
    }
    case 'switch_method':
      return <KvLine label="swap" value={`${kind.from} → ${kind.to}`} />
    case 'pin_library_version':
      return <KvLine label="pin" value={`${kind.library} ${kind.from ?? '?'} → ${kind.to}`} />
    case 'override_parameter':
      return <KvLine label="override" value={`${kind.param} = ${JSON.stringify(kind.to)}`} />
    case 'swap_input_data':
      return <KvLine label="swap input" value={`${kind.field}: ${kind.from ?? '?'} → ${kind.to}`} />
    case 'rerun_upstream':
      return <KvLine label="rerun" value={kind.producer_task_id} />
    case 'tweak_executor': {
      const flips: string[] = []
      if (kind.disable_spot) flips.push('disable_spot')
      if (kind.partition) flips.push(`partition=${kind.partition}`)
      if (kind.availability_zone) flips.push(`az=${kind.availability_zone}`)
      return <KvLine label="executor" value={flips.join(', ') || '(no flips)'} />
    }
    case 'retry_as_is':
      return <KvLine label="reason" value={kind.reason} />
    case 'rebuild_environment':
      return (
        <div style={{ marginTop: 6 }}>
          <KvLine label="capability" value={kind.capability} />
          <KvLine label="run" value={kind.operator_command_hint} mono />
        </div>
      )
    case 'manual_review':
      return (
        <div style={{ marginTop: 6, fontSize: 12, color: '#666' }}>
          {kind.suggested_next_steps.length > 0 && (
            <ul style={{ paddingLeft: 16, margin: '4px 0' }}>
              {kind.suggested_next_steps.map((s, i) => (
                <li key={i}>{s}</li>
              ))}
            </ul>
          )}
        </div>
      )
  }
}

function KvLine({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <div style={{ marginTop: 6, fontSize: 12 }}>
      <span style={{ color: '#666' }}>{label}: </span>
      <span style={{ fontFamily: mono ? 'monospace' : 'inherit', color: '#333' }}>{value}</span>
    </div>
  )
}

function kindHeading(k: RemediationKind): string {
  switch (k.kind) {
    case 'bump_resources':
      return 'Bump resources'
    case 'switch_method':
      return `Switch ${k.switch_kind.replace('_', ' ')}`
    case 'pin_library_version':
      return `Pin ${k.library}`
    case 'override_parameter':
      return `Override ${k.param}`
    case 'swap_input_data':
      return `Swap ${k.swap_kind} input`
    case 'rerun_upstream':
      return `Rerun upstream (${k.producer_task_id})`
    case 'tweak_executor':
      return 'Tweak executor'
    case 'retry_as_is':
      return 'Retry as-is'
    case 'rebuild_environment':
      return `Rebuild environment for ${k.capability}`
    case 'manual_review':
      return 'Manual review'
  }
}

function applyButtonLabel(s: RemediationSuggestion): string {
  switch (s.tool_binding) {
    case 'rerun_task':
    case 'rerun_upstream_task':
      return 'Apply & rerun'
    case 'amend_stage_method':
      return 'Record method swap'
    case 'set_intake_field':
      return 'Record input swap'
    case 'operator_action':
      return 'Mark as needs operator'
    case 'manual_only':
      return 'Acknowledge'
  }
}
