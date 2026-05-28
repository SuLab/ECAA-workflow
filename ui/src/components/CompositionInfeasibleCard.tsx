import type { BlockerKind, ExcludedPath } from '../types'

/**
 * `CompositionInfeasibleCard` UI component.
 *
 * Renders a dedicated structured affordance for the
 * `BlockerKind::CompositionInfeasible` variant the deterministic
 * composer surfaces when no chain reaches the SME's goal from the
 * available atom registry. The card unfolds three sections so the
 * SME has a concrete affordance instead of free-form prose:
 *
 * 1. **Unreachable goal** (when present) — the EDAM-typed goal the
 *  composer couldn't produce, rendered with the IRI alongside any
 *  human label so non-bioinformatician readers see both.
 * 2. **Missing inputs** — EDAM-typed missing-input list (per Galaxy
 *  LLM Hub Oct 2025 + arXiv 2507.20122). One row per missing input
 *  with the IRI as a monospace chip + "Try adding…" suggestion.
 * 3. **Excluded paths** — atoms the composer considered but ruled
 *  out via an `excludes:` CEL expression. Surfacing the rejected
 *  candidates is per Round-2 §3.10 ("EDAM-typed missing-input
 *  errors not free-text") + the BETSY pruning trace requirement.
 *
 * Rendered inline by `BlockerCard.tsx` when
 * `blockerKind.kind === 'composition_infeasible'`. The card itself
 * does not own the recovery button — the parent `BlockerCard`
 * provides the "Open composer recovery" affordance once the SME has
 * had a chance to read the structured detail.
 */
export default function CompositionInfeasibleCard({
  blockerKind,
}: {
  blockerKind: Extract<BlockerKind, { kind: 'composition_infeasible' }>
}) {
  const { unreachable_goal, missing_inputs, excluded_paths } = blockerKind
  const hasGoal = !!unreachable_goal
  const hasInputs = missing_inputs.length > 0
  const hasExcludes = excluded_paths.length > 0
  if (!hasGoal && !hasInputs && !hasExcludes) {
    return null
  }
  return (
    <div
      role="region"
      aria-label="Composer recovery details"
      style={{
        marginTop: '0.75rem',
        padding: '0.65rem 0.85rem',
        fontSize: '0.78rem',
        color: 'var(--color-text-secondary)',
        backgroundColor: 'var(--color-surface-muted)',
        borderRadius: '0.4rem',
        border: '1px solid var(--color-border-subtle)',
        lineHeight: 1.5,
      }}
    >
      {hasGoal && (
        <UnreachableGoalSection goal={unreachable_goal!} />
      )}
      {hasInputs && (
        <MissingInputsSection inputs={missing_inputs} />
      )}
      {hasExcludes && (
        <ExcludedPathsSection paths={excluded_paths} />
      )}
      <CompositionTabLink />
    </div>
  )
}

/**
 * Link out to the dedicated Composition tab where the SME
 * can see the full typed outcome (refusal report, novel-node spec,
 * adapter warnings, assumption ledger, ranked alternatives) without
 * leaving the session. Falls through harmlessly when the session is
 * on a legacy composer (the tab still renders, just shows the
 * legacy-composer notice).
 */
function CompositionTabLink() {
  return (
    <p
      style={{
        margin: '0.6rem 0 0',
        fontSize: '0.74rem',
        color: 'var(--color-text-muted)',
      }}
    >
      For the typed outcome (refusal / novel-node / ranked
      alternatives) and the assumption ledger, open the{' '}
      <strong>Composition</strong> tab in the right pane.
    </p>
  )
}

function UnreachableGoalSection({ goal }: { goal: string }) {
  return (
    <div style={{ marginBottom: '0.6rem' }}>
      <SectionHeading>Unreachable goal</SectionHeading>
      <div style={{ display: 'flex', alignItems: 'center', gap: '0.5rem' }}>
        <EdamChip iri={goal} />
        <span style={{ color: 'var(--color-text-muted)', fontSize: '0.74rem' }}>
          {edamHumanLabel(goal)}
        </span>
      </div>
      <p
        style={{
          margin: '0.35rem 0 0',
          fontSize: '0.74rem',
          color: 'var(--color-text-muted)',
          fontStyle: 'italic',
        }}
      >
        No atom chain reaches this data class from your current registry.
        Try adding an atom that produces this output, or pick a related
        goal that the registry can satisfy.
      </p>
    </div>
  )
}

function MissingInputsSection({ inputs }: { inputs: string[] }) {
  return (
    <div style={{ marginBottom: '0.6rem' }}>
      <SectionHeading>
        Missing input{inputs.length === 1 ? '' : 's'}
      </SectionHeading>
      <ul
        style={{
          margin: 0,
          paddingLeft: '1.1rem',
          listStyle: 'disc',
        }}
      >
        {inputs.map((inp) => (
          <li key={inp} style={{ marginBottom: '0.2rem' }}>
            <EdamChip iri={inp} />{' '}
            <span style={{ color: 'var(--color-text-muted)' }}>
              {edamHumanLabel(inp)}
            </span>
          </li>
        ))}
      </ul>
      <p
        style={{
          margin: '0.35rem 0 0',
          fontSize: '0.74rem',
          color: 'var(--color-text-muted)',
          fontStyle: 'italic',
        }}
      >
        Try adding an atom that produces {inputs.length === 1 ? 'this input' : 'these inputs'}, or
        an upstream stage that supplies the EDAM-typed slot.
      </p>
    </div>
  )
}

function ExcludedPathsSection({ paths }: { paths: ExcludedPath[] }) {
  return (
    <div>
      <SectionHeading>
        Excluded path{paths.length === 1 ? '' : 's'}
      </SectionHeading>
      <ul
        style={{
          margin: 0,
          paddingLeft: '1.1rem',
          listStyle: 'disc',
          fontFamily: 'ui-monospace, monospace',
          fontSize: '0.74rem',
        }}
      >
        {paths.map((p, idx) => (
          <li
            key={`${p.atom_id}-${idx}`}
            style={{ marginBottom: '0.2rem' }}
          >
            <span style={{ color: 'var(--color-text-primary)' }}>
              {p.atom_id}
            </span>
            {' — '}
            <code
              style={{
                color: 'var(--color-text-muted)',
                background: 'var(--color-surface-1)',
                padding: '0.05rem 0.25rem',
                borderRadius: '0.2rem',
              }}
            >
              {p.exclusion_cel}
            </code>
          </li>
        ))}
      </ul>
      <p
        style={{
          margin: '0.35rem 0 0',
          fontSize: '0.74rem',
          color: 'var(--color-text-muted)',
          fontStyle: 'italic',
        }}
      >
        These atoms were ruled out by the listed CEL expression. If you
        believe an exclusion is wrong, branch the session and edit the
        atom's <code style={{ fontFamily: 'inherit' }}>excludes:</code>{' '}
        block.
      </p>
    </div>
  )
}

function SectionHeading({ children }: { children: React.ReactNode }) {
  return (
    <h4
      style={{
        margin: '0 0 0.25rem 0',
        fontSize: '0.78rem',
        fontWeight: 600,
        color: 'var(--color-text-primary)',
      }}
    >
      {children}
    </h4>
  )
}

function EdamChip({ iri }: { iri: string }) {
  return (
    <code
      style={{
        background: 'var(--color-surface-1)',
        color: 'var(--color-text-primary)',
        padding: '0.1rem 0.3rem',
        borderRadius: '0.2rem',
        fontFamily: 'ui-monospace, monospace',
        fontSize: '0.74rem',
      }}
    >
      {iri}
    </code>
  )
}

/**
 * Tiny EDAM-IRI → human label table. Covers the IRIs the composer
 * surfaces today; future expansion either piggybacks on a server-side
 * label resolver or pulls from a shared IRI table. Returns empty
 * string for unknown IRIs (the chip alone is already informative).
 */
function edamHumanLabel(iri: string): string {
  const map: Record<string, string> = {
    'data:0951': 'Statistical estimate score',
    'data:1383': 'Sequence alignment',
    'data:2044': 'Sequence',
    'data:3917': 'Count matrix',
    'format:1929': 'FASTA',
    'format:1930': 'FASTQ',
    'format:2305': 'GFF',
    'format:3475': 'Tab-separated values (TSV)',
    'format:3590': 'HDF5',
    'operation:0292': 'Sequence alignment',
    'operation:3196': 'Genome assembly',
    'operation:3198': 'Read mapping',
    'operation:3223': 'Differential expression',
  }
  return map[iri] ?? ''
}
