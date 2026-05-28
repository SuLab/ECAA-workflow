// Vitest setup — runs once before each test file.
//
// Pulls in @testing-library/jest-dom matchers (toBeInTheDocument,
// toHaveAttribute, toHaveAccessibleName, etc.) so the assertions read
// the same as the ones you'd write in a Jest project. Vitest's globals
// option means tests don't need to import describe/it/expect.

import '@testing-library/jest-dom/vitest'
import { afterEach, vi } from 'vitest'
import { cleanup } from '@testing-library/react'

// react-virtuoso needs ResizeObserver plus non-zero element geometry before it
// will mount rows. jsdom has neither, so provide deterministic layout shims
// globally instead of repeating partial no-op stubs in each virtualized test.
if (typeof window !== 'undefined') {
  class ResizeObserverStub implements ResizeObserver {
    private callback: ResizeObserverCallback

    constructor(callback: ResizeObserverCallback) {
      this.callback = callback
    }

    observe(target: Element): void {
      const rect = target.getBoundingClientRect()
      const size = {
        blockSize: rect.height || 800,
        inlineSize: rect.width || 1024,
      } as ResizeObserverSize
      const entry = {
        target,
        contentRect: rect,
        borderBoxSize: [size],
        contentBoxSize: [size],
        devicePixelContentBoxSize: [size],
      } as ResizeObserverEntry
      queueMicrotask(() => this.callback([entry], this))
    }

    unobserve(): void {}
    disconnect(): void {}
  }

  Object.defineProperty(window, 'ResizeObserver', {
    writable: true,
    configurable: true,
    value: ResizeObserverStub,
  })
  Object.defineProperty(globalThis, 'ResizeObserver', {
    writable: true,
    configurable: true,
    value: ResizeObserverStub,
  })

  const originalGetBoundingClientRect =
    HTMLElement.prototype.getBoundingClientRect
  HTMLElement.prototype.getBoundingClientRect = function (): DOMRect {
    const r = originalGetBoundingClientRect.call(this) as DOMRect
    return new DOMRect(0, 0, r.width || 1024, r.height || 800)
  }

  Object.defineProperty(HTMLElement.prototype, 'offsetHeight', {
    configurable: true,
    get() {
      return 800
    },
  })
  Object.defineProperty(HTMLElement.prototype, 'offsetWidth', {
    configurable: true,
    get() {
      return 1024
    },
  })

  if (!HTMLElement.prototype.scrollTo) {
    HTMLElement.prototype.scrollTo = vi.fn()
  }
}

// jsdom does not implement window.matchMedia, but useTheme and any
// @media-driven code needs it. Stub a passive implementation that
// always resolves to "light" (no-match) and supports both the modern
// addEventListener API and the legacy addListener callback form.
if (typeof window !== 'undefined' && typeof window.matchMedia !== 'function') {
  Object.defineProperty(window, 'matchMedia', {
    writable: true,
    configurable: true,
    value: vi.fn().mockImplementation((query: string) => ({
      matches: false,
      media: query,
      onchange: null,
      addListener: vi.fn(),
      removeListener: vi.fn(),
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      dispatchEvent: vi.fn(),
    })),
  })
}

// Tear down anything React Testing Library mounted between tests so a
// stray DOM node from one test doesn't leak into the next.
afterEach(() => {
  cleanup()
})
