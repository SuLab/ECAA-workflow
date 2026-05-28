import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from 'react'
import { THEMES, type ThemeMode, type ThemeTokens } from '../styles/tokens'

export type ThemePreference = 'light' | 'dark' | 'system'

interface ThemeContextValue {
  preference: ThemePreference
  mode: ThemeMode
  tokens: ThemeTokens
  setPreference: (p: ThemePreference) => void
}

const STORAGE_KEY = 'swfc.theme'

const ThemeContext = createContext<ThemeContextValue | null>(null)

function readStoredPreference(): ThemePreference {
  if (typeof window === 'undefined') return 'system'
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY)
    if (raw === 'light' || raw === 'dark' || raw === 'system') return raw
  } catch {
    /* localStorage can throw in private-browsing / sandboxed iframes */
  }
  return 'system'
}

function systemMode(): ThemeMode {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') {
    return 'light'
  }
  return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'
}

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [preference, setPreferenceState] = useState<ThemePreference>(readStoredPreference)
  const [systemResolved, setSystemResolved] = useState<ThemeMode>(systemMode)

  useEffect(() => {
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return
    const mq = window.matchMedia('(prefers-color-scheme: dark)')
    const listener = (e: MediaQueryListEvent) => setSystemResolved(e.matches ? 'dark' : 'light')
    mq.addEventListener('change', listener)
    return () => mq.removeEventListener('change', listener)
  }, [])

  const mode: ThemeMode = preference === 'system' ? systemResolved : preference

  useEffect(() => {
    document.documentElement.dataset.theme = mode
  }, [mode])

  const setPreference = useCallback((p: ThemePreference) => {
    setPreferenceState(p)
    try {
      window.localStorage.setItem(STORAGE_KEY, p)
    } catch {
      /* silently ignore — preference reverts to system on next load */
    }
  }, [])

  const value = useMemo<ThemeContextValue>(
    () => ({ preference, mode, tokens: THEMES[mode], setPreference }),
    [preference, mode, setPreference],
  )

  return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>
}

export function useTheme(): ThemeContextValue {
  const ctx = useContext(ThemeContext)
  if (!ctx) {
    throw new Error('useTheme() called outside <ThemeProvider>')
  }
  return ctx
}
