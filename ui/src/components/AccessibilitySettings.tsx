// Accessibility settings dropdown in the title bar. Gear/person icon
// opens a panel with font size slider, high-contrast toggle,
// reduced-motion toggle, and color-blind-safe palette toggle. All
// persist to localStorage via `../lib/a11y.ts`.

import { useEffect, useRef, useState } from 'react'
import {
  applySettings,
  loadSettings,
  saveSettings,
  type A11ySettings,
} from '../lib/a11y'
import { Z } from '../lib/z-index'

export default function AccessibilitySettings(): JSX.Element {
  const [open, setOpen] = useState(false)
  const [settings, setSettings] = useState<A11ySettings>(() => loadSettings())
  const panelRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    applySettings(settings)
    saveSettings(settings)
  }, [settings])

  useEffect(() => {
    if (!open) return
    const onClick = (e: MouseEvent) => {
      if (!panelRef.current) return
      if (!panelRef.current.contains(e.target as Node)) setOpen(false)
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false)
    }
    document.addEventListener('mousedown', onClick)
    document.addEventListener('keydown', onKey)
    return () => {
      document.removeEventListener('mousedown', onClick)
      document.removeEventListener('keydown', onKey)
    }
  }, [open])

  const update = (patch: Partial<A11ySettings>) =>
    setSettings((cur) => ({ ...cur, ...patch }))

  return (
    <div ref={panelRef} style={{ position: 'relative' }}>
      <button
        type="button"
        aria-label="Accessibility settings"
        aria-expanded={open}
        title="Accessibility"
        onClick={() => setOpen((o) => !o)}
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
        ⛨
      </button>
      {open && (
        <div
          role="dialog"
          aria-label="Accessibility settings"
          style={{
            position: 'absolute',
            right: 0,
            top: 'calc(100% + 6px)',
            minWidth: 280,
            padding: '0.8rem',
            background: 'var(--color-surface-0)',
            border: '1px solid var(--color-border-default)',
            borderRadius: 6,
            boxShadow: '0 4px 16px rgba(0,0,0,0.12)',
            zIndex: Z.DROPDOWN,
            display: 'flex',
            flexDirection: 'column',
            gap: '0.7rem',
          }}
        >
          <label style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
            <span style={{ fontSize: '0.82rem', fontWeight: 600 }}>
              Font size: {Math.round(settings.fontScale * 100)}%
            </span>
            <input
              type="range"
              min={0.8}
              max={1.4}
              step={0.05}
              value={settings.fontScale}
              onChange={(e) =>
                update({ fontScale: parseFloat(e.target.value) })
              }
            />
          </label>
          <label style={checkRow}>
            <input
              type="checkbox"
              checked={settings.highContrast}
              onChange={(e) => update({ highContrast: e.target.checked })}
            />
            <span>High contrast</span>
          </label>
          <label style={checkRow}>
            <input
              type="checkbox"
              checked={settings.reducedMotion}
              onChange={(e) => update({ reducedMotion: e.target.checked })}
            />
            <span>Reduce motion</span>
          </label>
          <label style={checkRow}>
            <input
              type="checkbox"
              checked={settings.colorSafe}
              onChange={(e) => update({ colorSafe: e.target.checked })}
            />
            <span>Color-blind-safe palette</span>
          </label>
          <button
            type="button"
            onClick={() =>
              setSettings({
                fontScale: 1.0,
                highContrast: false,
                reducedMotion: false,
                colorSafe: false,
              })
            }
            style={{
              marginTop: '0.3rem',
              padding: '0.35rem 0.6rem',
              border: '1px solid var(--color-border-default)',
              background: 'var(--color-surface-1)',
              color: 'var(--color-text-secondary)',
              borderRadius: 4,
              fontSize: '0.78rem',
              cursor: 'pointer',
              alignSelf: 'flex-start',
            }}
          >
            Reset
          </button>
        </div>
      )}
    </div>
  )
}

const checkRow: React.CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  gap: '0.5rem',
  fontSize: '0.85rem',
  color: 'var(--color-text-primary)',
  cursor: 'pointer',
}
