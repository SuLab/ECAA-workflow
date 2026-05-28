import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  ReactFlow,
  Background,
  Controls,
  MarkerType,
  type Node,
  type Edge,
  type EdgeMouseHandler,
  type NodeMouseHandler,
} from '@xyflow/react'
import Dagre from '@dagrejs/dagre'
import type { DAG, Task } from '../types'
import { useTheme } from '../hooks/useTheme'
import { NODE_HOVER_DEBOUNCE_MS } from '../lib/polling'
import TaskCard, { type TaskCardData } from './TaskCard'

const NODE_TYPES = { taskCard: TaskCard }
const NODE_W = 180
const NODE_H = 68

/** Collapse iteration chains longer than this on the
 *  Plan canvas. The agent expands `<base>_iter_<N>` linearly at runtime
 *  per the iterate-until contract (S10.4); when N grows past 5 the chain
 *  visually dominates the DAG. The collapsed view keeps iter 1, the last
 *  two iters, and a single synthetic "+N more" badge node in between so
 *  the SME can still see entry/exit but isn't reading 30 visually
 *  identical compute boxes. */
const ITER_CHAIN_COLLAPSE_THRESHOLD = 5

type TaskNode = Node<TaskCardData>

function layoutNodes(nodes: TaskNode[], edges: Edge[]): TaskNode[] {
  const g = new Dagre.graphlib.Graph()
  g.setGraph({ rankdir: 'TB', ranksep: 55, nodesep: 28 })
  g.setDefaultEdgeLabel(() => ({}))
  nodes.forEach((n) => g.setNode(n.id, { width: NODE_W, height: NODE_H }))
  edges.forEach((e) => g.setEdge(e.source, e.target))
  Dagre.layout(g)
  return nodes.map((n) => {
    const { x, y } = g.node(n.id)
    return { ...n, position: { x: x - NODE_W / 2, y: y - NODE_H / 2 } }
  })
}

function buildAdjacencyLists(dag: DAG): {
  fwd: Record<string, string[]>
  rev: Record<string, string[]>
} {
  const fwd: Record<string, string[]> = {}
  const rev: Record<string, string[]> = {}
  for (const [id, t] of Object.entries(dag.tasks)) {
    if (!t) continue
    for (const dep of t.depends_on ?? []) {
      (fwd[id] ||= []).push(dep)
      ;(rev[dep] ||= []).push(id)
    }
  }
  return { fwd, rev }
}

function bfs(start: string, adj: Record<string, string[]>): Set<string> {
  const seen = new Set<string>()
  const q = [start]
  while (q.length) {
    const cur = q.shift()!
    for (const next of adj[cur] ?? []) {
      if (!seen.has(next)) {
        seen.add(next)
        q.push(next)
      }
    }
  }
  return seen
}

/**
 * Compute both the upstream (ancestors) and downstream (descendants)
 * dependency chain for a given task. Used by hover-highlight in the
 * Plan tab so the SME can see "what feeds this" + "what this feeds
 * into" without reading raw JSON.
 */
export function computeDependencyChain(dag: DAG, target: string): {
  upstream: Set<string>
  downstream: Set<string>
} {
  const { fwd, rev } = buildAdjacencyLists(dag)
  return { upstream: bfs(target, fwd), downstream: bfs(target, rev) }
}

const ITER_TASK_RE = /^(.+)_iter_(\d+)$/

/** Returns the (base, n) pair when `id` matches the runtime-expanded
 *  iterate-until naming convention `<base>_iter_<n>`, otherwise null.
 *  Exported for tests + for TaskCardData iter-count derivation. */
export function parseIterTaskId(id: string): { base: string; n: number } | null {
  const m = ITER_TASK_RE.exec(id)
  if (!m) return null
  const n = Number(m[2])
  if (!Number.isInteger(n) || n < 1) return null
  return { base: m[1]!, n }
}

interface IterCollapseInfo {
  /** Map from base id (e.g. `clustering`) to the full ordered list of
   *  iter task ids we observed in the DAG. */
  chains: Map<string, string[]>
  /** Set of original iter task ids the SVG must hide (replaced by the
   *  synthetic placeholder node). Empty when no chain crossed the
   *  collapse threshold. */
  hiddenIds: Set<string>
  /** Map from synthetic placeholder id (e.g. `clustering_iter_collapsed`)
   *  to its grouped metadata. The placeholder is wired into the layout
   *  in `dagToFlow` with the same dependency edges the hidden middle
   *  iters would have carried. */
  placeholders: Map<
    string,
    {
      base: string
      hiddenIters: number[]
      precedingIter: string
      followingIter: string
    }
  >
  /** Per-base count of total iter tasks observed, surfaced onto the
   *  iterate_check / placeholder TaskCardData so the SME sees "iter 7
   *  of 10" without expanding the chain. */
  iterCounts: Map<string, number>
}

/**
 * Scan a DAG for `<base>_iter_<N>` chains longer than
 * `ITER_CHAIN_COLLAPSE_THRESHOLD` and decide which iter ids to hide
 * + which synthetic "+N more" placeholder ids to insert. Pure data
 * computation — `dagToFlow` consumes the result to drive the actual
 * node + edge construction.
 */
export function planIterCollapse(dag: DAG): IterCollapseInfo {
  const chains = new Map<string, string[]>()
  const iterCounts = new Map<string, number>()
  for (const id of Object.keys(dag.tasks)) {
    const parsed = parseIterTaskId(id)
    if (!parsed) continue
    const list = chains.get(parsed.base) ?? []
    list.push(id)
    chains.set(parsed.base, list)
  }
  // Sort each chain by iter index so first/last picks are deterministic.
  for (const [base, list] of chains.entries()) {
    list.sort((a, b) => {
      const an = parseIterTaskId(a)?.n ?? 0
      const bn = parseIterTaskId(b)?.n ?? 0
      return an - bn
    })
    iterCounts.set(base, list.length)
  }

  const hiddenIds = new Set<string>()
  const placeholders = new Map<
    string,
    {
      base: string
      hiddenIters: number[]
      precedingIter: string
      followingIter: string
    }
  >()

  for (const [base, list] of chains.entries()) {
    if (list.length <= ITER_CHAIN_COLLAPSE_THRESHOLD) continue
    // Keep iter_1 + the last two iters. Hide the middle.
    // E.g., 8-iter chain becomes [1, +5 more, 7, 8].
    const keepLeading = 1
    const keepTrailing = 2
    const hidden = list.slice(keepLeading, list.length - keepTrailing)
    if (hidden.length === 0) continue
    for (const id of hidden) hiddenIds.add(id)
    const phId = `${base}_iter_collapsed`
    const precedingIter = list[keepLeading - 1]! // iter_1
    const followingIter = list[list.length - keepTrailing]! // first kept-trailing iter
    placeholders.set(phId, {
      base,
      hiddenIters: hidden
        .map((id) => parseIterTaskId(id)?.n ?? 0)
        .filter((n) => n > 0),
      precedingIter,
      followingIter,
    })
  }
  return { chains, hiddenIds, placeholders, iterCounts }
}

function dagToFlow(
  dag: DAG,
  activeId: string | null,
  hoverId: string | null,
  hoveredChain: { upstream: Set<string>; downstream: Set<string> } | null,
  edgeColors: { dim: string; bright: string },
  statusFilter: ReadonlySet<string> | null,
  recentHorizonMs: number,
  onActivateNode?: (id: string) => void,
  /**
   * Optional precomputed positions (id → {x, y}). When supplied and
   * covering every emitted node, skip Dagre.layout and reuse these
   * coordinates — the hot path when the structural signature of the
   * DAG hasn't changed and only per-task state (running → completed,
   * hover-driven dim toggles) is in flux.
   */
  cachedPositions?: Map<string, { x: number; y: number }>,
): { nodes: TaskNode[]; edges: Edge[] } {
  // The "kept bright" set for hover-highlight: the hovered node itself
  // plus its upstream (ancestors) + downstream (descendants). Without
  // explicitly adding `hoverId`, the target gets dimmed (BFS's `seen`
  // never contains `start`) and every edge to/from the target gets
  // dimmed too — the exact opposite of the "focus on this node's
  // dep chain" UX the hover is trying to achieve.
  const hovered = hoveredChain && hoverId
    ? new Set<string>([
        hoverId,
        ...hoveredChain.upstream,
        ...hoveredChain.downstream,
      ])
    : null
  // Pre-compute iter-chain collapse so the per-task loop
  // can decide which nodes to hide + which synthetic placeholder nodes
  // to inject. Pure deterministic transform; doesn't mutate the DAG.
  const collapse = planIterCollapse(dag)
  const now = Date.now()
  const rawNodes: TaskNode[] = Object.entries(dag.tasks)
    .filter((entry): entry is [string, Task] => entry[1] != null)
    .filter(([id]) => !collapse.hiddenIds.has(id))
    .map(([id, task]) => {
      const isActive = id === activeId
      // Hover-driven dim is intentionally dropped. When tasks are
      // running, the harness streams progress events that re-render
      // the dag; transient mouseleave/enter bounces during those
      // re-renders made the dim cascade flicker rhythmically across
      // 14+ nodes. Edge dimming (below) is sufficient to convey the
      // hovered chain; node-level opacity changes were the noise.
      const filterDim =
        statusFilter !== null && statusFilter.size > 0
          ? !nodeMatchesFilter(task, statusFilter, now, recentHorizonMs)
          : false
      // Surface the iter count on the iterate placeholder
      // and check nodes so the SME sees "iter 7" without expanding.
      // The base id is the placeholder's own id; for `iterate_check_<base>`
      // we strip the prefix to find the matching chain.
      const iterCount = (() => {
        if (collapse.iterCounts.has(id)) return collapse.iterCounts.get(id)
        if (id.startsWith('iterate_check_')) {
          return collapse.iterCounts.get(id.slice('iterate_check_'.length))
        }
        if (id.startsWith('iterate_gate_')) {
          return collapse.iterCounts.get(id.slice('iterate_gate_'.length))
        }
        return undefined
      })()
      return {
        id,
        type: 'taskCard',
        position: { x: 0, y: 0 },
        data: {
          task,
          active: isActive,
          dim: filterDim,
          onActivate: onActivateNode ? () => onActivateNode(id) : undefined,
          iterCount,
        } as TaskCardData,
      }
    })

  // Inject synthetic "+N more" placeholder nodes for each collapsed chain.
  // The placeholder is a Computation-shaped node carrying a synthetic
  // Task — TaskCard renders it the same way as a real task because the
  // shape matches; only the description carries the "+N more" label. This
  // keeps the layout engine happy without a separate node type.
  for (const [phId, info] of collapse.placeholders.entries()) {
    const syntheticTask: Task = {
      kind: 'computation',
      state: { status: 'completed' },
      depends_on: [info.precedingIter],
      assignee: 'agent',
      description: `+${info.hiddenIters.length} more iterations (collapsed)`,
      spec: { iterate_role: 'collapsed_segment', base: info.base },
      resolution: null,
      result_ref: null,
      resource_class: 'cpu_heavy',
      requires_sme_review: false,
      required_artifacts: [],
    } as unknown as Task
    rawNodes.push({
      id: phId,
      type: 'taskCard',
      position: { x: 0, y: 0 },
      data: {
        task: syntheticTask,
        active: false,
        dim: false,
        onActivate: undefined,
        collapsedChain: {
          base: info.base,
          hiddenCount: info.hiddenIters.length,
        },
      } as TaskCardData,
    })
  }

  const taskIds = new Set(rawNodes.map((n) => n.id))

  const edges: Edge[] = []
  // Re-route the edges: middle iters' `<prev>_iter_N → <next>_iter_N+1`
  // chain becomes `<base>_iter_1 → <base>_iter_collapsed → <follower>`.
  // We track which (source,target) pairs we've already emitted so the
  // synthetic placeholder gets one inbound + one outbound edge instead
  // of N edges proportional to the hidden chain length.
  const emittedEdges = new Set<string>()
  function pushEdge(source: string, target: string) {
    const key = `${source}→${target}`
    if (emittedEdges.has(key)) return
    if (!taskIds.has(source) || !taskIds.has(target)) return
    emittedEdges.add(key)
    const dim =
      hovered !== null && !(hovered.has(source) && hovered.has(target))
    edges.push({
      id: key,
      source,
      target,
      // `cursor: pointer` on edges signals that they're
      // clickable so SMEs discover the EdgeProofDrawer. The
      // interaction is wired via DagCanvas's `onEdgeClick` prop.
      style: {
        stroke: dim ? edgeColors.dim : edgeColors.bright,
        strokeWidth: 1.5,
        cursor: 'pointer',
      },
      // `interactionWidth` (reactflow) widens the hit area so a
      // 1.5px stroke stays clickable without forcing the user to
      // pixel-hunt. 12px gives a 6px halo on each side.
      interactionWidth: 12,
      markerEnd: {
        type: MarkerType.ArrowClosed,
        color: dim ? edgeColors.dim : edgeColors.bright,
        width: 12,
        height: 12,
      },
    })
  }
  for (const [id, task] of Object.entries(dag.tasks)) {
    if (!task) continue
    if (collapse.hiddenIds.has(id)) continue
    for (const dep of task.depends_on) {
      if (collapse.hiddenIds.has(dep)) {
        // Dep was elided — re-target the edge at the synthetic
        // placeholder for the same base. parseIterTaskId returns the
        // chain id; the placeholder id is `<base>_iter_collapsed`.
        const parsed = parseIterTaskId(dep)
        if (!parsed) continue
        pushEdge(`${parsed.base}_iter_collapsed`, id)
        continue
      }
      pushEdge(dep, id)
    }
  }
  // Wire the synthetic placeholders into the layout: iter_1 → placeholder
  // and placeholder → first kept-trailing iter (the second-to-last node
  // in the original chain). These edges carry the visual continuity the
  // hidden middle would have shown.
  for (const [phId, info] of collapse.placeholders.entries()) {
    pushEdge(info.precedingIter, phId)
    pushEdge(phId, info.followingIter)
  }

  // Cache hit: every emitted node has a known position, so skip the
  // dagre relayout and stamp positions in-place. Cache miss (any node
  // missing from the supplied map, including the first call before
  // any cache exists): fall back to the canonical Dagre.layout.
  let layouted: TaskNode[]
  if (cachedPositions && rawNodes.every((n) => cachedPositions.has(n.id))) {
    layouted = rawNodes.map((n) => ({
      ...n,
      position: cachedPositions.get(n.id)!,
    }))
  } else {
    layouted = layoutNodes(rawNodes, edges)
  }
  return { nodes: layouted, edges }
}

interface Props {
  dag: DAG
  /** The task whose TaskDetailDrawer is currently open. Renders an
   *  outline pulse on that node so the SME can see what the drawer is
   *  describing. */
  activeTaskId?: string | null
  /** Called when the SME clicks a task node. PlanTab lifts this to
   *  set activeTaskId + open the drawer. */
  onNodeClick?: (taskId: string) => void
  /** Called when the SME clicks an edge in the canvas.
   *  PlanTab lifts this to open the EdgeProofDrawer with the
   *  matching `EdgeContract` from `runtime/proofs.jsonl`. The
   *  edge id is the synthetic dagre id (`<from>->-<to>`); the
   *  caller resolves it back to a typed edge by from/to ids. */
  onEdgeClick?: (fromId: string, toId: string) => void
  /** When present and non-empty, nodes whose status isn't in the set
   *  are dimmed; the set itself is a combination of task statuses
   *  ('pending','ready','running','completed','failed','blocked')
   *  plus the synthetic 'recent' marker which matches tasks completed
   *  within recentHorizonMs. */
  statusFilter?: ReadonlySet<string> | null
  /** Horizon for the 'recent' marker in ms (default 1 hour). */
  recentHorizonMs?: number
}

const DEFAULT_RECENT_HORIZON_MS = 60 * 60 * 1000

function nodeMatchesFilter(
  task: Task,
  filter: ReadonlySet<string>,
  _now: number,
  _recentHorizonMs: number,
): boolean {
  const status = task.state?.status
  if (!status) return false
  // 'recent' is an alias for 'completed' until the DAG JSON carries
  // per-task completion timestamps. Group them together so the chip
  // set stays forward-compatible.
  if (filter.has(status)) return true
  if (filter.has('recent') && status === 'completed') return true
  return false
}

export default function DagCanvas({
  dag,
  activeTaskId,
  onNodeClick,
  onEdgeClick,
  statusFilter,
  recentHorizonMs = DEFAULT_RECENT_HORIZON_MS,
}: Props) {
  const [nodes, setNodes] = useState<TaskNode[]>([])
  const [edges, setEdges] = useState<Edge[]>([])
  const [hoverId, setHoverId] = useState<string | null>(null)
  // ReactFlow <Background color=...> and edge markers resolve colors as
  // SVG presentation attributes, so they can't pull from CSS custom
  // properties. Read the JS-side theme tokens and pass the concrete
  // hex through so the canvas swaps on theme change.
  const { tokens } = useTheme()
  const edgeColors = useMemo(
    () => ({ dim: tokens.borderDefault, bright: tokens.textFaint }),
    [tokens.borderDefault, tokens.textFaint],
  )

  // Memoize the structural signature so ReactFlow doesn't fully remount
  // on every state-advance event (per-task property updates keep the
  // same key; task add/remove flips it).
  const dagKey = useMemo(
    () => Object.keys(dag.tasks).sort().join(','),
    [dag],
  )

  // Only fit the viewport when the DAG structure actually changes.
  // Without this, every state-advance refetch destroys manual pan/zoom
  // even though the layout hasn't changed.
  const lastFitKeyRef = useRef<string | null>(null)
  const shouldFitView = lastFitKeyRef.current !== dagKey
  if (shouldFitView) {
    lastFitKeyRef.current = dagKey
  }

  // Memoize adjacency lists per dag so hover-highlight BFS doesn't
  // rebuild them on every mouse move.
  const adjacency = useMemo(() => buildAdjacencyLists(dag), [dag])
  const hoveredChain = useMemo(() => {
    if (!hoverId) return null
    return {
      upstream: bfs(hoverId, adjacency.fwd),
      downstream: bfs(hoverId, adjacency.rev),
    }
  }, [adjacency, hoverId])

  // Cache the most recent dagre-computed positions so when the DAG's
  // structural signature is unchanged we can skip Dagre.layout (the
  // hot path on every harness state-advance) and just re-derive the
  // per-node `data` (status colors, active flag, hover dim) using the
  // already-known coordinates. Dagre layout is the single biggest cost
  // in dagToFlow — skipping it on color-only re-renders turns a
  // ~20-task DAG re-render from ~12ms into ~2ms.
  const lastLayoutKeyRef = useRef<string>('')
  const lastPositionsRef = useRef<Map<string, { x: number; y: number }>>(new Map())

  useEffect(() => {
    const structureUnchanged =
      lastLayoutKeyRef.current === dagKey && lastPositionsRef.current.size > 0
    const { nodes: n, edges: e } = dagToFlow(
      dag,
      activeTaskId ?? null,
      hoverId,
      hoveredChain,
      edgeColors,
      statusFilter ?? null,
      recentHorizonMs,
      onNodeClick,
      structureUnchanged ? lastPositionsRef.current : undefined,
    )
    if (!structureUnchanged) {
      lastLayoutKeyRef.current = dagKey
      const fresh = new Map<string, { x: number; y: number }>()
      for (const node of n) fresh.set(node.id, node.position)
      lastPositionsRef.current = fresh
    }
    setNodes(n)
    setEdges(e)
  }, [dag, dagKey, activeTaskId, hoverId, hoveredChain, edgeColors, statusFilter, recentHorizonMs, onNodeClick])

  const handleNodeClick: NodeMouseHandler = useCallback(
    (_ev, node) => {
      onNodeClick?.(node.id)
    },
    [onNodeClick],
  )
  const handleEdgeClick: EdgeMouseHandler<Edge> = useCallback(
    (_ev, edge) => {
      // Surface edge clicks so the consumer (PlanTab)
      // can open the EdgeProofDrawer. We pass source/target ids
      // separately because edge ids are synthesized by dagre and
      // the consumer needs the typed (from, to) tuple to look up
      // the matching proof from runtime/proofs.jsonl.
      if (edge.source && edge.target) {
        onEdgeClick?.(edge.source, edge.target)
      }
    },
    [onEdgeClick],
  )
  // Debounce hover state to absorb transient mouseleave/enter bounces
  // that fire when ReactFlow re-renders nodes mid-execution. A 250ms
  // window lets the cursor "rest" on a node — enough for the user to
  // see edge highlighting, but short enough that intentional moves
  // still feel responsive.
  const hoverClearTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const handleNodeMouseEnter: NodeMouseHandler = useCallback((_ev, node) => {
    if (hoverClearTimerRef.current) {
      clearTimeout(hoverClearTimerRef.current)
      hoverClearTimerRef.current = null
    }
    setHoverId(node.id)
  }, [])
  const handleNodeMouseLeave: NodeMouseHandler = useCallback(() => {
    if (hoverClearTimerRef.current) clearTimeout(hoverClearTimerRef.current)
    hoverClearTimerRef.current = setTimeout(() => {
      setHoverId(null)
      hoverClearTimerRef.current = null
    }, NODE_HOVER_DEBOUNCE_MS)
  }, [])
  useEffect(() => {
    return () => {
      if (hoverClearTimerRef.current) clearTimeout(hoverClearTimerRef.current)
    }
  }, [])

  return (
    <ReactFlow
      key={dagKey}
      nodes={nodes}
      edges={edges}
      onNodesChange={() => {}}
      onEdgesChange={() => {}}
      onNodeClick={handleNodeClick}
      onEdgeClick={handleEdgeClick}
      onNodeMouseEnter={handleNodeMouseEnter}
      onNodeMouseLeave={handleNodeMouseLeave}
      nodeTypes={NODE_TYPES}
      fitView={shouldFitView}
      fitViewOptions={{ padding: 0.15 }}
      nodesDraggable={false}
      nodesConnectable={false}
      elementsSelectable={false}
      // paneClickDistance gives a 5px threshold so natural mouse
      // jitter between pointerdown and pointerup registers as a click,
      // not a drag. Without this, even a 1-2px slip during click would
      // be interpreted as a pan-start. With panOnDrag default (left-
      // mouse), users still get the expected click+drag-to-pan on
      // empty canvas.
      paneClickDistance={5}
      proOptions={{ hideAttribution: true }}
      style={{ background: tokens.surface0 }}
    >
      <Background color={tokens.grid} gap={24} />
      <Controls showInteractive={false} />
    </ReactFlow>
  )
}
