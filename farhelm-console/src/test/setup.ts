import '@testing-library/jest-dom/vitest'

import { queryClient } from '../api/cache'
import { cleanup } from '@testing-library/react'
import { afterEach, vi } from 'vitest'

afterEach(() => { cleanup(); queryClient.clear() })

Object.defineProperty(window, 'matchMedia', {
  writable: true,
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

class ResizeObserverMock {
  observe() {}
  unobserve() {}
  disconnect() {}
}

globalThis.ResizeObserver = ResizeObserverMock

class EventSourceMock extends EventTarget {
  url: string
  constructor(url: string) { super(); this.url = url }
  close() {}
}
Object.defineProperty(globalThis, 'EventSource', { configurable: true, writable: true, value: EventSourceMock })
