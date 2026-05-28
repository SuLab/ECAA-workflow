// Global undo stack for reversible mutations. Each push records the
// prior state the UI needs to revert; the caller drives the undo
// action. The stack holds at most one token — latest mutation wins.
// Tokens auto-expire after 30s so the toast disappears on its own
// even if the caller forgets to clear it.

import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react'

export type UndoKind = 'amend' | 'rerun' | 'branch'

export interface UndoToken {
  kind: UndoKind
  label: string
  /** The async callback that reverses the mutation. */
  undo: () => Promise<void>
  createdAt: number
}

const WINDOW_MS = 30_000

interface Ctx {
  token: UndoToken | null
  push: (t: Omit<UndoToken, 'createdAt'>) => void
  clear: () => void
}

const UndoContext = createContext<Ctx | null>(null)

export function UndoStackProvider({ children }: { children: ReactNode }) {
  const [token, setToken] = useState<UndoToken | null>(null)
  const timerRef = useRef<number | null>(null)

  const clear = useCallback(() => {
    if (timerRef.current !== null) {
      window.clearTimeout(timerRef.current)
      timerRef.current = null
    }
    setToken(null)
  }, [])

  const push = useCallback<Ctx['push']>(
    (t) => {
      if (timerRef.current !== null) window.clearTimeout(timerRef.current)
      const stamped: UndoToken = { ...t, createdAt: Date.now() }
      setToken(stamped)
      timerRef.current = window.setTimeout(() => {
        setToken(null)
        timerRef.current = null
      }, WINDOW_MS)
    },
    [],
  )

  const value = useMemo(() => ({ token, push, clear }), [token, push, clear])
  return <UndoContext.Provider value={value}>{children}</UndoContext.Provider>
}

export function useUndoStack(): Ctx {
  const c = useContext(UndoContext)
  if (!c) throw new Error('useUndoStack() outside UndoStackProvider')
  return c
}

export const UNDO_WINDOW_MS = WINDOW_MS
