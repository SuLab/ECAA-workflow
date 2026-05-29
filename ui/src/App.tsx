import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react'
import AccessibilitySettings from './components/AccessibilitySettings'
import BudgetChip from './components/BudgetChip'
import CommandPalette from './components/CommandPalette'
import ConversationPane from './components/ConversationPane'
import ErrorBoundary from './components/ErrorBoundary'
import NeedsInputChip from './components/NeedsInputChip'
import NotificationOptInChip from './components/NotificationOptInChip'
import RecentSessionsDropdown from './components/RecentSessionsDropdown'
import SessionTitleBar from './components/SessionTitleBar'
import SessionTree from './components/SessionTree'
import ShareModal from './components/ShareModal'
import StateInspectorPane from './components/StateInspectorPane'
import UndoToast from './components/UndoToast'
import GitSettingsPage from './components/settings/GitSettingsPage'
import { EventsProvider, SessionProvider } from './hooks/contexts'
import { useConversation } from './hooks/useConversation'
import { useSseChatEvents } from './hooks/useSseChatEvents'
import { useTheme } from './hooks/useTheme'
import { UndoStackProvider } from './hooks/useUndoStack'
import { useViewport } from './hooks/useViewport'
import { isReadOnly } from './hooks/useReadOnly'
import {
  StreamingTextProvider,
  useStreamingTextControls,
} from './state/StreamingTextContext'
import type { SessionState, Turn } from './types'

const DESKTOP_BREAKPOINT = 1024
const TABLET_BREAKPOINT = 768

export default function App() {
  const { width } = useViewport()
  const isDesktop = width >= DESKTOP_BREAKPOINT
  const isTablet = width >= TABLET_BREAKPOINT && !isDesktop
  const [mobileView, setMobileView] = useState<'chat' | 'state'>('chat')
  // Top-level view toggle. Gear icon in the title bar flips us to
  // Settings; the back button inside the page flips us back. Routed via
  // ?view=settings so bookmark / browser-back / share-link work. Same
  // shape as ?session= and ?share_token=.
  const [topView, setTopViewState] = useState<'chat' | 'settings'>(() => {
    if (typeof window === 'undefined') return 'chat'
    return new URLSearchParams(window.location.search).get('view') === 'settings'
      ? 'settings'
      : 'chat'
  })

  const setTopView = useCallback((next: 'chat' | 'settings') => {
    setTopViewState(next)
    if (typeof window === 'undefined') return
    const params = new URLSearchParams(window.location.search)
    if (next === 'settings') params.set('view', 'settings')
    else params.delete('view')
    const qs = params.toString()
    const url =
      window.location.pathname + (qs ? `?${qs}` : '') + window.location.hash
    history.pushState(null, '', url)
  }, [])
  const [shareOpen, setShareOpen] = useState(false)
  const readOnly = isReadOnly()

  // When a keyboard user activates the mobile Chat / View plan toggle,
  // focus should move into the newly-visible pane instead of staying on
  // the toggle button. Pane refs + a useEffect that fires on mobileView
  // change handle this without stealing focus when the user switches to
  // a pane via mouse click on the toggle (the toggle button still has
  // focus until the effect moves it).
  const chatPaneRef = useRef<HTMLDivElement>(null)
  const statePaneRef = useRef<HTMLDivElement>(null)
  const previousMobileView = useRef<'chat' | 'state'>('chat')

  const { mode: themeMode, setPreference: setThemePreference } = useTheme()

  // Conversation lifecycle lives at the App level so the chat pane and
  // the state inspector pane share the same session. SSE subscription
  // is hoisted into `SseChatEventsBridge` below so it can consume
  // `useStreamingText()` from inside `<StreamingTextProvider>` — that
  // keeps `assistant_token_delta` re-renders scoped to the streaming
  // bubble subtree instead of bouncing through `App.tsx`.
  const conv = useConversation()

  useEffect(() => {
    void conv.start()
  // eslint-disable-next-line react-hooks/exhaustive-deps -- conv identity changes on every render; dep on .start is intentional
  }, [conv.start])

  useEffect(() => {
    const handler = () => setTopView('settings')
    window.addEventListener('swfc:open-settings', handler)
    return () => window.removeEventListener('swfc:open-settings', handler)
  }, [setTopView])

  // popstate (browser back/forward) keeps URL ?view= in sync with state.
  useEffect(() => {
    const handler = () => {
      const v = new URLSearchParams(window.location.search).get('view')
      setTopViewState(v === 'settings' ? 'settings' : 'chat')
    }
    window.addEventListener('popstate', handler)
    return () => window.removeEventListener('popstate', handler)
  }, [])

  // Reset mobile view to chat-first whenever the breakpoint changes back to
  // mobile after a desktop resize, so a returning user lands on the chat.
  useEffect(() => {
    if (!isDesktop && !isTablet) setMobileView('chat')
  }, [isDesktop, isTablet])

  // Move focus into the newly-visible pane after a mobile toggle. Skip on
  // the very first render so we don't steal focus from the initial
  // ChatComposer auto-focus, and skip on desktop where both panes are
  // visible at once.
  useEffect(() => {
    if (isDesktop) {
      previousMobileView.current = mobileView
      return
    }
    if (previousMobileView.current === mobileView) return
    previousMobileView.current = mobileView
    const target = mobileView === 'chat' ? chatPaneRef.current : statePaneRef.current
    if (target) {
      // tabindex -1 on the wrapper makes it focusable without inserting
      // it into the natural tab order.
      target.focus({ preventScroll: false })
    }
  }, [mobileView, isDesktop])

  const titleBar = (
    <div
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: '0.75rem',
        padding: '0.5rem 1rem',
        background: 'var(--color-chrome-bg)',
        color: 'var(--color-chrome-fg)',
        flexShrink: 0,
        borderBottom: '1px solid var(--color-chrome-border)',
      }}
    >
      <strong style={{ fontSize: '0.95rem', letterSpacing: '-0.01em' }}>
        ECAA-workflow
      </strong>
      <span
        style={{
          fontSize: '0.72rem',
          color: 'var(--color-chrome-fg-faint)',
          padding: '0.1rem 0.45rem',
          background: 'var(--color-chrome-bg-elevated)',
          borderRadius: 999,
        }}
      >
        natural chat
      </span>
      {conv.sessionId && (
        <span style={{ color: 'var(--color-chrome-fg-faint)', fontSize: '0.72rem' }}>
          session: <code
            data-testid="session-id-prefix"
            data-session-id={conv.sessionId}
            style={{ color: 'var(--color-chrome-fg-accent)' }}
          >{conv.sessionId.slice(0, 8)}</code>
        </span>
      )}
      <SessionTitleBar
        sessionId={conv.sessionId}
        title={conv.state?.title ?? null}
        turnCount={conv.turns.length}
        onTitled={() => {
          void conv.refreshCurrentState()
        }}
      />
      <NeedsInputChip
        blockedTasks={conv.state?.blocked_tasks ?? []}
        onJump={(taskId) => {
          // Plan tab owns the drawer; use the same URL-hash deep-link
          // contract DecisionsTab uses when it jumps to a task.
          window.location.hash = `task=${encodeURIComponent(taskId)}`
          // Swap to desktop Plan tab on small screens so the drawer can render.
          if (!isDesktop) {
            setMobileView('state')
          }
        }}
      />
      <NotificationOptInChip
        everBlocked={(conv.state?.blocked_tasks?.length ?? 0) > 0}
      />
      <BudgetChip sessionId={conv.sessionId} />
      <SessionTree
        currentSessionId={conv.sessionId}
        parentSessionId={conv.state?.parent_session_id ?? null}
        onSelectSession={conv.switchToSession}
      />
      <div style={{ flex: 1 }} />
      <RecentSessionsDropdown
        currentSessionId={conv.sessionId}
        onSelect={(id) => {
          void conv.switchToSession(id)
        }}
        onNewSession={() => {
          // Drop the URL ?session= so reset() lands on a fresh greeting
          // instead of re-attaching to the session we just left.
          if (typeof window !== 'undefined') {
            const url = new URL(window.location.href)
            url.searchParams.delete('session')
            window.history.pushState(null, '', url.toString())
          }
          void conv.reset()
        }}
      />
      {readOnly && (
        <span
          role="status"
          aria-live="polite"
          style={{
            padding: '0.2rem 0.55rem',
            marginRight: '0.4rem',
            fontSize: '0.7rem',
            fontWeight: 600,
            color: 'var(--color-warning-fg)',
            background: 'var(--color-warning-bg)',
            border: '1px solid var(--color-warning-border)',
            borderRadius: 999,
          }}
          title="Read-only view — ask the session owner to make changes."
        >
          Read-only
        </span>
      )}
      {!readOnly && conv.sessionId && (
        <button
          type="button"
          aria-label="Share session"
          title="Share session (read-only link)"
          onClick={() => setShareOpen(true)}
          style={{
            background: 'transparent',
            border: '1px solid var(--color-chrome-border-strong)',
            color: 'var(--color-chrome-fg-muted)',
            padding: '0.3rem 0.55rem',
            borderRadius: 4,
            cursor: 'pointer',
            fontSize: '0.72rem',
            marginRight: '0.35rem',
          }}
        >
          Share
        </button>
      )}
      <AccessibilitySettings />
      <button
        type="button"
        aria-label={themeMode === 'dark' ? 'Switch to light theme' : 'Switch to dark theme'}
        title={themeMode === 'dark' ? 'Switch to light theme' : 'Switch to dark theme'}
        onClick={() => setThemePreference(themeMode === 'dark' ? 'light' : 'dark')}
        style={{
          background: 'transparent',
          border: '1px solid var(--color-chrome-border-strong)',
          color: 'var(--color-chrome-fg-muted)',
          padding: '0.3rem 0.5rem',
          borderRadius: 4,
          cursor: 'pointer',
          fontSize: '0.95rem',
          marginRight: '0.35rem',
        }}
      >
        {themeMode === 'dark' ? '☀' : '☾'}
      </button>
      <button
        type="button"
        aria-label={topView === 'chat' ? 'Open Settings' : 'Back to chat'}
        title="Settings"
        onClick={() =>
          setTopView(topView === 'chat' ? 'settings' : 'chat')
        }
        style={{
          background: 'transparent',
          border: '1px solid var(--color-chrome-border-strong)',
          color: 'var(--color-chrome-fg-muted)',
          padding: '0.3rem 0.5rem',
          borderRadius: 4,
          cursor: 'pointer',
          fontSize: '0.95rem',
          marginRight: '0.5rem',
        }}
      >
        {topView === 'chat' ? '⚙' : '←'}
      </button>
      {!isDesktop && (
        <div
          role="tablist"
          aria-label="Mobile view switcher"
          style={{ display: 'flex', gap: '0.25rem' }}
        >
          {(['chat', 'state'] as const).map((v) => (
            <button
              key={v}
              type="button"
              role="tab"
              aria-selected={mobileView === v}
              onClick={() => setMobileView(v)}
              style={{
                padding: '0.3rem 0.7rem',
                background: mobileView === v ? 'var(--color-accent)' : 'transparent',
                color: mobileView === v ? 'var(--color-accent-fg)' : 'var(--color-chrome-fg-muted)',
                border: '1px solid var(--color-chrome-border-strong)',
                borderRadius: 999,
                cursor: 'pointer',
                fontSize: '0.75rem',
                fontWeight: 600,
              }}
            >
              {v === 'chat' ? 'Chat' : 'View plan'}
            </button>
          ))}
        </div>
      )}
    </div>
  )

  return (
    <SessionProvider value={conv}>
      <StreamingTextProvider>
        <SseChatEventsBridge
          sessionId={conv.sessionId}
          onStateAdvanced={conv.applyStateAdvanced}
          onResyncRequired={conv.resyncAll}
          onTurnAppended={conv.appendTurn}
        >
        <UndoStackProvider>
        <div style={{ display: 'flex', flexDirection: 'column', height: '100vh' }}>
          {titleBar}
          <div style={{ flex: 1, minHeight: 0, display: 'flex', overflow: 'hidden' }}>
            {topView === 'settings' ? (
              <div style={{ flex: 1, display: 'flex', flexDirection: 'column' }}>
                <ErrorBoundary fallbackLabel="settings">
                  <GitSettingsPage onClose={() => setTopView('chat')} />
                </ErrorBoundary>
              </div>
            ) : isDesktop ? (
              <>
                <div
                  style={{
                    width: '45%',
                    minWidth: 360,
                    display: 'flex',
                    flexDirection: 'column',
                  }}
                >
                  <ErrorBoundary fallbackLabel="the chat">
                    <ConversationPane />
                  </ErrorBoundary>
                </div>
                <div style={{ flex: 1, display: 'flex', flexDirection: 'column' }}>
                  <ErrorBoundary fallbackLabel="the state inspector">
                    <StateInspectorPane />
                  </ErrorBoundary>
                </div>
              </>
            ) : (
              <div style={{ flex: 1, display: 'flex', flexDirection: 'column' }}>
                <div
                  ref={chatPaneRef}
                  tabIndex={-1}
                  aria-label="Chat pane"
                  style={{
                    display: mobileView === 'chat' ? 'flex' : 'none',
                    flexDirection: 'column',
                    flex: 1,
                    minHeight: 0,
                    outline: 'none',
                  }}
                >
                  <ErrorBoundary fallbackLabel="the chat">
                    <ConversationPane />
                  </ErrorBoundary>
                </div>
                <div
                  ref={statePaneRef}
                  tabIndex={-1}
                  aria-label="State inspector pane"
                  style={{
                    display: mobileView === 'state' ? 'flex' : 'none',
                    flexDirection: 'column',
                    flex: 1,
                    minHeight: 0,
                    outline: 'none',
                  }}
                >
                  <ErrorBoundary fallbackLabel="the state inspector">
                    <StateInspectorPane />
                  </ErrorBoundary>
                </div>
              </div>
            )}
          </div>
          <UndoToast />
          <CommandPalette sessionId={conv.sessionId} />
          {shareOpen && (
            <ShareModal
              sessionId={conv.sessionId}
              onClose={() => setShareOpen(false)}
            />
          )}
        </div>
        </UndoStackProvider>
        </SseChatEventsBridge>
      </StreamingTextProvider>
    </SessionProvider>
  )
}

/**
 * Adapter that owns the single `useSseChatEvents` subscription. Sits
 * inside `<StreamingTextProvider>` so it can pull the rAF-coalesced
 * `append` / `reset` callbacks from the streaming-text context and
 * thread them into the SSE hook. Wraps its `children` in
 * `<EventsProvider>` so consumers downstream still resolve
 * `useEventsContext()` the same way.
 *
 * `App.tsx` itself does NOT subscribe to `useStreamingText`, so
 * `assistant_token_delta` events that bump the streaming text only
 * re-render components that explicitly read from the streaming-text
 * context (the in-flight bubble).
 */
function SseChatEventsBridge({
  sessionId,
  onStateAdvanced,
  onResyncRequired,
  onTurnAppended,
  children,
}: {
  sessionId: string | null
  onStateAdvanced: (newState?: SessionState) => void | Promise<void>
  onResyncRequired: (dropped: number) => void | Promise<void>
  onTurnAppended: (turn: Turn) => void
  children: ReactNode
}): JSX.Element {
  // `useStreamingTextControls()` returns stable-identity callbacks
  // (the sibling context held separately from the text value), so
  // this bridge does NOT re-render on every committed streaming
  // frame. That keeps
  // `<EventsProvider>`'s value object identity-stable across deltas, so
  // `useEventsContext()` consumers (StateInspectorPane, etc.) don't
  // bounce on streaming text. `useSseChatEvents` opts are captured via
  // `optsRef` inside the hook so the EventSource doesn't churn when
  // callback identities change.
  const { append, reset } = useStreamingTextControls()
  const sse = useSseChatEvents(sessionId, {
    onStateAdvanced,
    onResyncRequired,
    onTurnAppended,
    appendStreamingText: append,
    resetStreamingText: reset,
  })
  return <EventsProvider value={sse}>{children}</EventsProvider>
}
