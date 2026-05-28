import { createContext, useContext, type ReactNode } from 'react'
import type { useConversation } from './useConversation'
import type { useSseChatEvents } from './useSseChatEvents'

/**
 * `SessionContext` and `EventsContext` wrap the single `useConversation`
 * + `useSseChatEvents` instances owned by App.tsx. Consumers pull state
 * via `useSessionContext()` / `useEventsContext()` instead of threading
 * `conv` / `sse` props through every pane. App.tsx still owns the hook
 * lifecycle — the contexts only broadcast the already-running hook
 * returns.
 */

type SessionValue = ReturnType<typeof useConversation>
type EventsValue = ReturnType<typeof useSseChatEvents>

export const SessionContext = createContext<SessionValue | null>(null)
export const EventsContext = createContext<EventsValue | null>(null)

export function SessionProvider({
  value,
  children,
}: {
  value: SessionValue
  children: ReactNode
}): JSX.Element {
  return <SessionContext.Provider value={value}>{children}</SessionContext.Provider>
}

export function EventsProvider({
  value,
  children,
}: {
  value: EventsValue
  children: ReactNode
}): JSX.Element {
  return <EventsContext.Provider value={value}>{children}</EventsContext.Provider>
}

export function useSessionContext(): SessionValue {
  const v = useContext(SessionContext)
  if (!v) {
    throw new Error(
      'useSessionContext called outside of SessionProvider — wrap the tree in App.tsx',
    )
  }
  return v
}

export function useEventsContext(): EventsValue {
  const v = useContext(EventsContext)
  if (!v) {
    throw new Error(
      'useEventsContext called outside of EventsProvider — wrap the tree in App.tsx',
    )
  }
  return v
}
