/**
 * AgentCodeSubtab — renders the per-task LLM-generated code record.
 *
 * Displays prompt / response text / executed code from the
 * `agent-code.json` sidecar written by agent-claude.sh.  When the
 * sidecar is absent (`agentCode` is null/undefined) the component
 * renders a muted "not available" placeholder so the parent doesn't
 * need to gate on presence before mounting.
 */

import type { AgentCodeRecord } from '../api/chatClient'

interface Props {
  agentCode: AgentCodeRecord | null | undefined
}

const MONO_BLOCK: React.CSSProperties = {
  fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
  fontSize: '0.82rem',
  lineHeight: 1.55,
  background: 'var(--color-surface-0, #f8f9fa)',
  border: '1px solid var(--color-border-subtle, #e2e8f0)',
  borderRadius: 4,
  padding: '0.65rem 0.75rem',
  whiteSpace: 'pre-wrap',
  wordBreak: 'break-word',
  overflowX: 'auto',
  color: 'var(--color-text-primary, #1a202c)',
  maxHeight: '18rem',
  overflowY: 'auto',
}

const FIELD_LABEL: React.CSSProperties = {
  fontSize: '0.72rem',
  fontWeight: 600,
  textTransform: 'uppercase',
  letterSpacing: '0.06em',
  color: 'var(--color-text-muted, #718096)',
  marginBottom: 4,
}

const FIELD_BLOCK: React.CSSProperties = {
  marginBottom: '1rem',
}

const META_ROW: React.CSSProperties = {
  display: 'flex',
  gap: '1.5rem',
  fontSize: '0.78rem',
  color: 'var(--color-text-muted, #718096)',
  marginBottom: '1rem',
  flexWrap: 'wrap',
}

function LanguageBadge({ language }: { language: string }): JSX.Element {
  const colors: Record<string, { bg: string; fg: string }> = {
    Python: { bg: '#edf7ed', fg: '#276221' },
    R: { bg: '#eaf4fb', fg: '#1a5276' },
    Bash: { bg: '#fefde8', fg: '#7d6608' },
  }
  const c = colors[language] ?? { bg: 'var(--color-surface-0)', fg: 'var(--color-text-muted)' }
  return (
    <span
      style={{
        display: 'inline-block',
        padding: '0.12rem 0.5rem',
        borderRadius: 12,
        fontSize: '0.72rem',
        fontWeight: 600,
        background: c.bg,
        color: c.fg,
        border: '1px solid rgba(0,0,0,0.06)',
      }}
    >
      {language}
    </span>
  )
}

function formatTs(ts: string): string {
  try {
    return new Date(ts).toLocaleString(undefined, {
      year: 'numeric',
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
    })
  } catch {
    return ts
  }
}

export default function AgentCodeSubtab({ agentCode }: Props): JSX.Element {
  if (!agentCode) {
    return (
      <div
        data-testid="agent-code-absent"
        style={{
          padding: '2rem 0',
          textAlign: 'center',
          color: 'var(--color-text-muted, #718096)',
          fontSize: '0.85rem',
        }}
      >
        Code capture not available for this task.
      </div>
    )
  }

  return (
    <div data-testid="agent-code-subtab" style={{ padding: '0.5rem 0' }}>
      <div style={META_ROW}>
        <span>
          <strong style={{ fontWeight: 600 }}>Language:</strong>{' '}
          <LanguageBadge language={agentCode.language} />
        </span>
        <span>
          <strong style={{ fontWeight: 600 }}>Started:</strong>{' '}
          <time dateTime={agentCode.started_at}>{formatTs(agentCode.started_at)}</time>
        </span>
        <span>
          <strong style={{ fontWeight: 600 }}>Completed:</strong>{' '}
          <time dateTime={agentCode.completed_at}>{formatTs(agentCode.completed_at)}</time>
        </span>
      </div>

      {agentCode.executed_code.length > 0 && (
        <div style={FIELD_BLOCK}>
          <div style={FIELD_LABEL}>Executed code</div>
          <pre
            data-testid="agent-code-executed"
            style={MONO_BLOCK}
          >
            {agentCode.executed_code}
          </pre>
        </div>
      )}

      {agentCode.response_text.length > 0 && (
        <div style={FIELD_BLOCK}>
          <div style={FIELD_LABEL}>Agent response</div>
          <pre data-testid="agent-code-response" style={MONO_BLOCK}>
            {agentCode.response_text}
          </pre>
        </div>
      )}

      {agentCode.prompt.length > 0 && (
        <div style={FIELD_BLOCK}>
          <div style={FIELD_LABEL}>Prompt sent to agent</div>
          <pre data-testid="agent-code-prompt" style={MONO_BLOCK}>
            {agentCode.prompt}
          </pre>
        </div>
      )}
    </div>
  )
}
