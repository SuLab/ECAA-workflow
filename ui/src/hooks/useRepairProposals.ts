// V3+v4 residuals closure repair-proposal hook.
//
// Polls `GET /api/chat/session/:id/repair/pending` on a 4s cadence so the
// `RepairsTab` surfaces new proposals shortly after the planner emits
// them (the substrate is process-wide; the tab acts on whatever the
// drain returns). The accept/reject helpers wrap the chat-client POSTs
// and trigger a manual refresh through the returned `refresh` function.
//
// The 4s cadence is deliberately conservative: repair proposals land in
// bursts (one per gap) and stay until the SME acts. Tighter polling
// would just hammer the substrate drain without changing the perceived
// latency.

import { useCallback, useEffect, useState } from 'react'
import {
  acceptRepair,
  fetchRepairProposals,
  rejectRepair,
} from '../api/chatClient'
import { REPAIR_PROPOSALS_POLL_MS } from '../lib/polling'
import type { RepairProposal } from '../types/RepairProposal'

export interface UseRepairProposalsResult {
  /** Current pending proposals; empty until the first fetch completes. */
  proposals: RepairProposal[]
  /** Last fetch error message; null when the fetch path is healthy. */
  error: string | null
  /** True until the first fetch returns (covers the empty-by-default case). */
  loading: boolean
  /**
   * Accept a proposal. Optional credentials chain — the server returns
   * 403 if the proposal's `required_credentials` aren't a subset.
   */
  accept: (proposalId: string, credentials?: string[], rationale?: string) => Promise<void>
  /**
   * Reject a proposal with a free-text reason (required; empty strings
   * are rejected by the server with 400).
   */
  reject: (proposalId: string, reason: string) => Promise<void>
  /** Manually re-fetch the pending list. */
  refresh: () => Promise<void>
}

/**
 * Reusable repair-proposal hook. Returns the pending proposals plus
 * accept/reject/refresh helpers. Polls every 4 seconds while mounted;
 * unmounting clears the interval and aborts in-flight refreshes.
 */
export function useRepairProposals(
  sessionId: string | null,
): UseRepairProposalsResult {
  const [proposals, setProposals] = useState<RepairProposal[]>([])
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState<boolean>(true)

  const load = useCallback(async () => {
    if (!sessionId) {
      setProposals([])
      setLoading(false)
      return
    }
    try {
      const next = await fetchRepairProposals(sessionId)
      setProposals(next)
      setError(null)
    } catch (e) {
      setError((e as Error).message)
    } finally {
      setLoading(false)
    }
  }, [sessionId])

  useEffect(() => {
    let cancelled = false
    void (async () => {
      if (cancelled) return
      await load()
    })()
    // Gate polling on document.visibilityState so a backgrounded tab
    // doesn't hammer the substrate-drain endpoint.
    const handle = window.setInterval(() => {
      if (cancelled) return
      if (document.visibilityState !== 'visible') return
      void load()
    }, REPAIR_PROPOSALS_POLL_MS)
    return () => {
      cancelled = true
      window.clearInterval(handle)
    }
  }, [load])

  const accept = useCallback(
    async (proposalId: string, credentials?: string[], rationale?: string) => {
      if (!sessionId) return
      await acceptRepair(sessionId, proposalId, credentials ?? [], rationale)
    },
    [sessionId],
  )

  const reject = useCallback(
    async (proposalId: string, reason: string) => {
      if (!sessionId) return
      await rejectRepair(sessionId, proposalId, reason)
    },
    [sessionId],
  )

  return { proposals, error, loading, accept, reject, refresh: load }
}
