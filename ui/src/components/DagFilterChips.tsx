// Horizontal chip row above DagCanvas that lets the SME filter by task
// status. Multi-select: chips combine additively — empty selection
// means "show all". Selection is persisted to localStorage so the
// filter sticks across reloads.

import { useEffect, useState } from 'react'

const STORAGE_KEY = 'ecaa.dagFilter.statuses'

export const FILTER_CHOICES: ReadonlyArray<{ id: string; label: string }> = [
  { id: 'blocked', label: 'Blocked' },
  { id: 'running', label: 'Running' },
  { id: 'ready', label: 'Ready to run' },
  { id: 'failed', label: 'Failed' },
  { id: 'recent', label: 'Completed' },
]

export function loadFilter(): Set<string> {
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY)
    if (!raw) return new Set()
    const arr = JSON.parse(raw)
    if (!Array.isArray(arr)) return new Set()
    return new Set(arr.filter((x): x is string => typeof x === 'string'))
  } catch {
    return new Set()
  }
}

function saveFilter(s: Set<string>): void {
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify([...s]))
  } catch {
    // ignore
  }
}

interface Props {
  selected: Set<string>
  onChange: (next: Set<string>) => void
}

export default function DagFilterChips({ selected, onChange }: Props) {
  const [showAll, setShowAll] = useState<boolean>(() => selected.size === 0)

  useEffect(() => {
    setShowAll(selected.size === 0)
  }, [selected])

  const toggle = (id: string) => {
    const next = new Set(selected)
    if (next.has(id)) next.delete(id)
    else next.add(id)
    saveFilter(next)
    onChange(next)
  }
  const clear = () => {
    saveFilter(new Set())
    onChange(new Set())
  }

  return (
    <div
      role="group"
      aria-label="Filter DAG by task status"
      style={{
        display: 'flex',
        gap: '0.4rem',
        flexWrap: 'wrap',
        padding: '0.4rem 0.6rem',
        borderBottom: '1px solid var(--color-border-default)',
        background: 'var(--color-surface-1)',
      }}
    >
      <button
        type="button"
        aria-pressed={showAll}
        onClick={clear}
        style={chipStyle(showAll)}
      >
        All
      </button>
      {FILTER_CHOICES.map((c) => {
        const on = selected.has(c.id)
        return (
          <button
            key={c.id}
            type="button"
            aria-pressed={on}
            onClick={() => toggle(c.id)}
            style={chipStyle(on)}
          >
            {c.label}
          </button>
        )
      })}
    </div>
  )
}

function chipStyle(active: boolean): React.CSSProperties {
  return {
    padding: '0.2rem 0.65rem',
    borderRadius: 999,
    border: active
      ? '1px solid var(--color-accent)'
      : '1px solid var(--color-border-default)',
    background: active ? 'var(--color-accent)' : 'var(--color-surface-0)',
    color: active ? 'var(--color-accent-fg)' : 'var(--color-text-secondary)',
    fontSize: '0.72rem',
    fontWeight: 600,
    cursor: 'pointer',
  }
}
