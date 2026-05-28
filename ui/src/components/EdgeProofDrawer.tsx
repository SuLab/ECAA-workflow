/**
 * `EdgeProofDrawer` UI component.
 *
 * When the SME clicks an edge in the DAG canvas, this drawer
 * renders the `CompatibilityProof` produced by the v4 planner. The
 * drawer answers "why does this edge exist?" with concrete proof
 * provenance: type subsumption path, facet matches, inserted
 * adapters, validators required at runtime, warnings, and the
 * free-text rationale.
 *
 * The drawer is read-only. Edits go through the existing
 * `amend_stage_method` / `propose_hypothesized_node` paths.
 *
 * Accessibility: closes on Escape, traps focus while open,
 * restores focus to the previously-focused element on close.
 */

import { useEffect, useRef } from 'react'

import type { AuditorProcedure } from '../types/AuditorProcedure'
import type { ChainOfCustody } from '../types/ChainOfCustody'
import type { ContentCommitment } from '../types/ContentCommitment'
import { Z } from '../lib/z-index'

export type { AuditorProcedure, ChainOfCustody, ContentCommitment }

export interface ProofFacetMatch {
  facet: string
  producer: string
  consumer: string
  kind: 'exact' | 'subtype' | 'substituted' | 'unknown'
  rationale?: string
}

/**
 * v3 P5 — `ChainOfCustody` shape rendered when a proof carries
 * suppressed content. The runtime shape is owned by
 * `crates/core/src/workflow_contracts/chain_of_custody.rs` and
 * propagated into `ui/src/types/{ChainOfCustody,AuditorProcedure,
 * ContentCommitment}.ts` via `make types`. The re-exports above
 * preserve the legacy `import { ChainOfCustody } from './EdgeProofDrawer'`
 * surface used by `AssumptionLedgerCard` and the proof tests.
 */

export interface EdgeProof {
  from_node: string
  from_port: string
  to_node: string
  to_port: string
  producer_type: string
  consumer_type: string
  ontology_subsumption_path: string[]
  facet_matches: ProofFacetMatch[]
  inserted_adapter_node_ids: string[]
  warnings: string[]
  rationale?: string
  /** v3 P5 — populated when the edge carries suppressed content. */
  chain_of_custody?: ChainOfCustody
}

interface Props {
  proof: EdgeProof | null
  onClose: () => void
}

export default function EdgeProofDrawer({ proof, onClose }: Props) {
  const drawerRef = useRef<HTMLElement | null>(null)
  const closeButtonRef = useRef<HTMLButtonElement | null>(null)
  const restoreFocusRef = useRef<HTMLElement | null>(null)

  // Capture the previously-focused element when the drawer opens
  // and restore it on close. Triggered by `proof` flipping
  // null → non-null (open) and back.
  useEffect(() => {
    if (!proof) return
    restoreFocusRef.current =
      (document.activeElement as HTMLElement | null) ?? null
    // Initial focus on the close button so keyboard users can
    // dismiss immediately. Defer one tick so the drawer mounts
    // before focus moves.
    const t = window.setTimeout(() => closeButtonRef.current?.focus(), 0)
    return () => {
      window.clearTimeout(t)
      restoreFocusRef.current?.focus?.()
    }
  }, [proof])

  // Escape closes; Tab cycles focus inside the drawer (focus trap).
  useEffect(() => {
    if (!proof) return
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault()
        onClose()
        return
      }
      if (e.key !== 'Tab') return
      const root = drawerRef.current
      if (!root) return
      const focusable = root.querySelectorAll<HTMLElement>(
        'a[href], button:not([disabled]), [tabindex]:not([tabindex="-1"]), input:not([disabled]), textarea:not([disabled])',
      )
      if (focusable.length === 0) return
      const first = focusable[0]
      const last = focusable[focusable.length - 1]
      const active = document.activeElement as HTMLElement | null
      if (e.shiftKey && active === first) {
        e.preventDefault()
        last!.focus()
      } else if (!e.shiftKey && active === last) {
        e.preventDefault()
        first!.focus()
      }
    }
    document.addEventListener('keydown', onKeyDown)
    return () => document.removeEventListener('keydown', onKeyDown)
  }, [proof, onClose])

  if (!proof) return null
  return (
    <aside
      ref={drawerRef}
      role="dialog"
      aria-modal="true"
      aria-label="Edge compatibility proof"
      style={drawerStyle}
    >
      <header style={headerStyle}>
        <strong style={{ fontSize: '0.85rem' }}>Why this edge exists</strong>
        <button
          ref={closeButtonRef}
          onClick={onClose}
          style={closeStyle}
          aria-label="Close drawer"
        >
          ×
        </button>
      </header>
      <div style={bodyStyle}>
        <Row label="From">
          {proof.from_node}
          <code style={codeStyle}>:{proof.from_port}</code>
        </Row>
        <Row label="To">
          {proof.to_node}
          <code style={codeStyle}>:{proof.to_port}</code>
        </Row>
        <Row label="Producer type">
          <code style={codeStyle}>{proof.producer_type}</code>
        </Row>
        <Row label="Consumer type">
          <code style={codeStyle}>{proof.consumer_type}</code>
        </Row>
        {proof.ontology_subsumption_path.length > 0 && (
          <Section title="Ontology subsumption path">
            <code style={pathStyle}>
              {proof.ontology_subsumption_path.join(' ▸ ')}
            </code>
          </Section>
        )}
        {proof.facet_matches.length > 0 && (
          <Section title="Facet matches">
            <ul style={facetListStyle}>
              {proof.facet_matches.map((f, i) => (
                <li key={`${f.facet}-${i}`}>
                  <code style={codeStyle}>{f.facet}</code>:{' '}
                  <span>{f.producer}</span> → <span>{f.consumer}</span>{' '}
                  <FacetKindChip kind={f.kind} />
                  {f.rationale && (
                    <em style={{ marginLeft: '0.4rem', color: 'var(--color-text-muted)' }}>
                      {f.rationale}
                    </em>
                  )}
                </li>
              ))}
            </ul>
          </Section>
        )}
        {proof.inserted_adapter_node_ids.length > 0 && (
          <Section title="Inserted adapters">
            <ul style={adapterListStyle}>
              {proof.inserted_adapter_node_ids.map((id) => (
                <li key={id}>
                  <code style={codeStyle}>{id}</code>
                </li>
              ))}
            </ul>
          </Section>
        )}
        {proof.warnings.length > 0 && (
          <Section title="Warnings">
            <ul style={warningListStyle}>
              {proof.warnings.map((w, i) => (
                <li key={i}>{w}</li>
              ))}
            </ul>
          </Section>
        )}
        {proof.rationale && (
          <Section title="Rationale">
            <p style={{ margin: 0, fontSize: '0.78rem' }}>{proof.rationale}</p>
          </Section>
        )}
        {proof.chain_of_custody && (
          <Section title="Chain of custody">
            <ChainOfCustodyPanel custody={proof.chain_of_custody} />
          </Section>
        )}
      </div>
    </aside>
  )
}

/**
 * v3 P5 — render the six chain-of-custody fields per §10.2. Used by
 * both `EdgeProofDrawer` and `AssumptionLedgerCard`; the styling is
 * shared so the two cards present custody consistently.
 */
export function ChainOfCustodyPanel({ custody }: { custody: ChainOfCustody }) {
  return (
    <dl style={custodyDlStyle}>
      <dt style={custodyDtStyle}>Suppression class</dt>
      <dd style={custodyDdStyle}>
        <code style={codeStyle}>{custody.suppression_class}</code>
      </dd>
      <dt style={custodyDtStyle}>Component</dt>
      <dd style={custodyDdStyle}>
        <code style={codeStyle}>{custody.suppressing_component}</code>
      </dd>
      <dt style={custodyDtStyle}>Timestamp</dt>
      <dd style={custodyDdStyle}>
        <time dateTime={custody.suppression_timestamp}>
          {custody.suppression_timestamp}
        </time>
      </dd>
      <dt style={custodyDtStyle}>Policy rule</dt>
      <dd style={custodyDdStyle}>
        <code style={codeStyle}>{custody.policy_rule_id}</code>
      </dd>
      {custody.cryptographic_commitment && (
        <>
          <dt style={custodyDtStyle}>
            Commitment ({custody.cryptographic_commitment.algorithm})
          </dt>
          <dd style={custodyDdStyle}>
            <code style={{ ...codeStyle, wordBreak: 'break-all' }}>
              {custody.cryptographic_commitment.digest_hex}
            </code>
          </dd>
        </>
      )}
      <dt style={custodyDtStyle}>Auditor access</dt>
      <dd style={custodyDdStyle}>
        <AuditorProcedureView procedure={custody.auditor_access} />
      </dd>
    </dl>
  )
}

function AuditorProcedureView({ procedure }: { procedure: AuditorProcedure }) {
  switch (procedure.kind) {
    case 'restricted_archive':
      return (
        <span>
          Restricted archive · ticket{' '}
          <code style={codeStyle}>{procedure.ticket_template ?? ''}</code>
        </span>
      )
    case 'permanently_deleted':
      return (
        <span>
          Permanently deleted · authority{' '}
          <code style={codeStyle}>{procedure.deletion_authority ?? ''}</code> ·{' '}
          deletion id{' '}
          <code style={codeStyle}>{procedure.deletion_id ?? ''}</code>
        </span>
      )
    case 'authenticated_api':
      return (
        <span>
          Authenticated API ·{' '}
          <code style={codeStyle}>{procedure.endpoint ?? ''}</code> · scope{' '}
          <code style={codeStyle}>{procedure.scope ?? ''}</code>
        </span>
      )
    default:
      return <span>(unknown procedure)</span>
  }
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div style={{ display: 'flex', gap: '0.4rem', alignItems: 'baseline' }}>
      <span style={labelStyle}>{label}</span>
      <span>{children}</span>
    </div>
  )
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div style={{ marginTop: '0.6rem' }}>
      <strong style={{ fontSize: '0.78rem', display: 'block', marginBottom: '0.2rem' }}>
        {title}
      </strong>
      {children}
    </div>
  )
}

function FacetKindChip({ kind }: { kind: ProofFacetMatch['kind'] }) {
  const color =
    kind === 'exact'
      ? 'var(--color-success-accent)'
      : kind === 'subtype'
        ? 'var(--color-success-fg)'
        : kind === 'substituted'
          ? 'var(--color-warning-accent)'
          : 'var(--color-danger-accent)'
  return (
    <span
      style={{
        background: color,
        color: '#fff',
        padding: '0.05rem 0.3rem',
        borderRadius: '0.3rem',
        fontSize: '0.7rem',
        marginLeft: '0.2rem',
      }}
    >
      {kind}
    </span>
  )
}

const drawerStyle: React.CSSProperties = {
  position: 'fixed',
  top: 0,
  right: 0,
  width: 'min(420px, 95vw)',
  height: '100vh',
  background: 'var(--color-surface-1)',
  borderLeft: '1px solid var(--color-border-subtle)',
  boxShadow: '-2px 0 12px rgba(0,0,0,0.18)',
  zIndex: Z.TOP,
  display: 'flex',
  flexDirection: 'column',
}
const headerStyle: React.CSSProperties = {
  display: 'flex',
  justifyContent: 'space-between',
  alignItems: 'center',
  padding: '0.75rem 1rem',
  borderBottom: '1px solid var(--color-border-subtle)',
}
const closeStyle: React.CSSProperties = {
  background: 'transparent',
  border: 'none',
  cursor: 'pointer',
  fontSize: '1.4rem',
  lineHeight: 1,
}
const bodyStyle: React.CSSProperties = {
  flex: 1,
  overflowY: 'auto',
  padding: '0.75rem 1rem',
  fontSize: '0.78rem',
  display: 'flex',
  flexDirection: 'column',
  gap: '0.3rem',
}
const labelStyle: React.CSSProperties = {
  fontWeight: 600,
  minWidth: '7rem',
}
const codeStyle: React.CSSProperties = {
  fontFamily: 'ui-monospace, monospace',
  background: 'var(--color-surface-muted)',
  padding: '0.05rem 0.3rem',
  borderRadius: '0.2rem',
  fontSize: '0.74rem',
}
const pathStyle: React.CSSProperties = {
  fontFamily: 'ui-monospace, monospace',
  fontSize: '0.74rem',
  display: 'block',
  whiteSpace: 'pre-wrap',
  wordBreak: 'break-word',
}
const facetListStyle: React.CSSProperties = {
  margin: 0,
  paddingLeft: '1.1rem',
  fontSize: '0.74rem',
}
const adapterListStyle: React.CSSProperties = {
  margin: 0,
  paddingLeft: '1.1rem',
  fontSize: '0.74rem',
}
const warningListStyle: React.CSSProperties = {
  margin: 0,
  paddingLeft: '1.1rem',
  fontSize: '0.74rem',
  color: 'var(--color-danger-accent)',
}
const custodyDlStyle: React.CSSProperties = {
  margin: 0,
  display: 'grid',
  gridTemplateColumns: '8rem 1fr',
  rowGap: '0.25rem',
  columnGap: '0.5rem',
  fontSize: '0.74rem',
}
const custodyDtStyle: React.CSSProperties = {
  fontWeight: 600,
  color: 'var(--color-text-muted)',
}
const custodyDdStyle: React.CSSProperties = {
  margin: 0,
}
