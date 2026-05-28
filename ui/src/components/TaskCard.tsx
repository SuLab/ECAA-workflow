import { Handle, Position, type NodeProps } from '@xyflow/react'
import type { MouseEvent, PointerEvent } from 'react'
import type { Task } from '../types'
import SafetyBadge from './SafetyBadge'

/**
 * DagCanvas passes a structured `TaskCardData` to every node: the raw
 * Task plus a couple of view hints. Using a dedicated type keeps
 * ReactFlow's type assertions happy without casting inside the canvas.
 */
export interface TaskCardData extends Record<string, unknown> {
  task: Task
  /** Rendered with a blue pulse outline when the TaskDetailDrawer is
   *  open on this node. */
  active?: boolean
  /** Dimmed (low-opacity) when hover-highlight isolates a different
   *  dependency chain. */
  dim?: boolean
  /** Click / Enter / Space activation. Threaded by DagCanvas in
   *  dagToFlow so keyboard users can open the TaskDetailDrawer
   *  (ReactFlow's onNodeClick is mouse-only by default). */
  onActivate?: () => void
  /** Number of `<base>_iter_<N>` tasks DagCanvas counted
   *  for an iterate-until placeholder / gate / check task. Surfaced as
   *  a compact "iter N" badge so the SME sees how far the loop has gone
   *  without expanding the chain. Absent on non-iterate nodes. */
  iterCount?: number
  /** Set on the synthetic "+N more" placeholder node
   *  DagCanvas injects when an iter chain crosses
   *  `ITER_CHAIN_COLLAPSE_THRESHOLD`. Drives the dashed-border + "+N
   *  more" badge styling that signals "this isn't a real task — it's a
   *  collapsed segment of a longer chain". */
  collapsedChain?: { base: string; hiddenCount: number }
}

// Status palette resolves to CSS vars (see tokens.css) so the node
// recolors on theme change. `blocked` shares the warning family with
// `running` visually — the glyph + label below keep them
// distinguishable.
const BG: Record<string, string> = {
  pending: 'var(--color-surface-2)',
  ready: 'var(--color-info-bg)',
  running: 'var(--color-warning-bg)',
  completed: 'var(--color-success-bg)',
  failed: 'var(--color-danger-bg)',
  blocked: 'var(--color-warning-bg)',
}

const BORDER: Record<string, string> = {
  pending: 'var(--color-border-strong)',
  ready: 'var(--color-info-accent)',
  running: 'var(--color-warning-accent)',
  completed: 'var(--color-success-accent)',
  failed: 'var(--color-danger-accent)',
  blocked: 'var(--color-warning-accent)',
}

// Status icons paired with the color ramp above. Symbols + color +
// text means the health cue is still readable on monochrome displays
// and to color-blind users (a11y requirement).
const STATUS_ICON: Record<string, string> = {
  pending: '⏸',
  ready: '⏱',
  running: '▶',
  completed: '✓',
  failed: '✗',
  blocked: '⚠',
}

const STATUS_FRIENDLY: Record<string, string> = {
  pending: 'Waiting',
  ready: 'Ready',
  running: 'Running',
  completed: 'Done',
  failed: 'Failed',
  blocked: 'Needs input',
}

function kindLabel(kind: Task['kind']): string {
  if (kind === 'computation') return 'compute'
  if (kind === 'validation') return 'validate'
  if (kind === 'review') return 'review'
  if (kind === 'gate') return 'gate'
  if (typeof kind === 'object' && 'discovery' in kind) return 'discover'
  return String(kind)
}

/**
 * Read the iterate-until role discriminator the builder
 * stamps onto each iterate-stage task's spec (S10.3). One of `gate`,
 * `placeholder`, `check`, `validate`, or our synthetic
 * `collapsed_segment` for hidden chain runs. Returns null on non-iterate
 * tasks so the visual treatment stays opt-in.
 */
function iterateRole(spec: unknown): string | null {
  if (!spec || typeof spec !== 'object') return null
  const role = (spec as Record<string, unknown>).iterate_role
  return typeof role === 'string' ? role : null
}

export default function TaskCard({
  data,
  id,
}: NodeProps & { data: TaskCardData }): JSX.Element {
  const task = data.task
  const status = task.state.status
  const bg = BG[status] ?? 'var(--color-surface-2)'
  const border = BORDER[status] ?? 'var(--color-border-strong)'
  const active = data.active === true
  const dim = data.dim === true
  const collapsed = data.collapsedChain
  const friendlyStatus = STATUS_FRIENDLY[status] ?? status
  const role = iterateRole(task.spec)

  const outline = active
    ? '0 0 0 3px var(--color-accent-muted-border), 0 0 0 6px var(--color-accent-muted-bg)'
    : undefined

  // Collapsed-segment placeholders get a dashed border so
  // the SME visually distinguishes "+N more" from a real task. The
  // dashed style is the standard "this is a stand-in" affordance shared
  // with collapsed dashboards / charts.
  const borderStyle = collapsed ? 'dashed' : 'solid'
  const stopPointer = (e: PointerEvent<HTMLElement>) => {
    e.stopPropagation()
  }
  const activate = (e: MouseEvent<HTMLElement>) => {
    e.stopPropagation()
    data.onActivate?.()
  }

  return (
    <>
      <Handle
        type="target"
        position={Position.Top}
        className="nopan nodrag"
        onPointerDown={stopPointer}
        onClick={activate}
        style={{
          background: 'var(--color-text-faint)',
          width: 7,
          height: 7,
          pointerEvents: 'auto',
        }}
      />
      <div
        role="button"
        tabIndex={0}
        aria-label={`Task ${id} — ${friendlyStatus}`}
        // nopan + nodrag are React Flow's built-in escape hatches.
        // Stop pointer propagation on the card/handle so single-clicks
        // reach onActivate cleanly without promoting to canvas pan.
        className="nopan nodrag"
        onPointerDown={stopPointer}
        onClick={activate}
        onKeyDown={(e) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault()
            data.onActivate?.()
          }
        }}
        style={{
          padding: '5px 9px',
          background: bg,
          border: `2px ${borderStyle} ${border}`,
          borderRadius: 6,
          minWidth: 155,
          maxWidth: 195,
          fontSize: '0.72rem',
          userSelect: 'none',
          cursor: 'pointer',
          opacity: dim ? 0.35 : 1,
          boxShadow: outline,
          transition: 'opacity 140ms ease, box-shadow 140ms ease',
        }}
        data-iter-role={role ?? undefined}
        data-collapsed-chain={collapsed ? 'true' : undefined}
      >
        <div
          style={{
            fontWeight: 600,
            color: 'var(--color-text-primary)',
            marginBottom: 2,
            overflow: 'hidden',
            textOverflow: 'ellipsis',
            whiteSpace: 'nowrap',
            fontSize: '0.73rem',
            display: 'flex',
            alignItems: 'center',
            gap: 4,
          }}
        >
          <span
            style={{
              overflow: 'hidden',
              textOverflow: 'ellipsis',
              whiteSpace: 'nowrap',
              flex: '1 1 auto',
              minWidth: 0,
            }}
          >
            {collapsed ? `+${collapsed.hiddenCount} more iterations` : id}
          </span>
          {/* Plan §A.S6 (Phase 8.2) — surface per-task safety classification
              on every TaskCard. Real tasks always carry a SafetyPolicy
              threaded from the source atom at emit time; the synthetic
              "+N more" collapsed-chain placeholder DagCanvas injects has
              no atom origin, so we skip the badge there (the dashed
              border + "+N more" label is already the dominant signal). */}
          {!collapsed && task.safety && (
            <SafetyBadge safety={task.safety} />
          )}
        </div>
        <div style={{ display: 'flex', gap: 4, alignItems: 'center' }}>
          <span
            style={{
              padding: '1px 4px',
              background: 'rgba(0,0,0,0.07)',
              borderRadius: 3,
              color: 'var(--color-text-secondary)',
              fontSize: '0.68rem',
            }}
          >
            {collapsed ? `${collapsed.base} chain` : kindLabel(task.kind)}
          </span>
          {role && !collapsed && (
            // Tiny role badge so the SME sees gate /
            // placeholder / check / validate at a glance on iterate
            // atoms. The four roles share a palette tint (info accent)
            // so the visual pairing reads as "this is the same loop".
            <span
              data-iter-role-badge={role}
              title={`iterate-until: ${role}`}
              style={{
                padding: '1px 4px',
                background: 'var(--color-info-bg)',
                borderRadius: 3,
                color: 'var(--color-info-fg)',
                fontSize: '0.62rem',
                fontWeight: 600,
                textTransform: 'uppercase',
                letterSpacing: '0.04em',
              }}
            >
              {role === 'placeholder' ? 'iter' : role}
            </span>
          )}
          {data.iterCount && data.iterCount > 0 && !collapsed && (
            <span
              data-iter-count={data.iterCount}
              title={`${data.iterCount} iterations spawned at runtime`}
              style={{
                padding: '1px 4px',
                background: 'var(--color-surface-2)',
                borderRadius: 3,
                color: 'var(--color-text-secondary)',
                fontSize: '0.62rem',
                fontWeight: 600,
              }}
            >
              ×{data.iterCount}
            </span>
          )}
          <span
            aria-hidden
            style={{
              color: border,
              fontSize: '0.75rem',
              fontWeight: 600,
              marginLeft: 'auto',
            }}
            title={friendlyStatus}
          >
            {STATUS_ICON[status] ?? '•'}
          </span>
          <span
            style={{
              color: border,
              fontSize: '0.68rem',
              fontWeight: 500,
            }}
          >
            {friendlyStatus}
          </span>
        </div>
        {task.description && (
          <div
            style={{
              marginTop: 2,
              color: 'var(--color-text-muted)',
              fontSize: '0.67rem',
              overflow: 'hidden',
              textOverflow: 'ellipsis',
              whiteSpace: 'nowrap',
            }}
          >
            {task.description}
          </div>
        )}
      </div>
      <Handle
        type="source"
        position={Position.Bottom}
        style={{ background: 'var(--color-text-faint)', width: 7, height: 7 }}
      />
    </>
  )
}
