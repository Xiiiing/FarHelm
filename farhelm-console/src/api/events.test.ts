import { expect, test, vi } from 'vitest'
import { readEventConnection, subscribeEventConnection, subscribeEvents } from './events'

test('connection status shares the live stream and resets after the authenticated page closes', () => {
  const sources: Source[] = []
  class Source extends EventTarget {
    closed = false
    constructor() { super(); sources.push(this) }
    close() { this.closed = true }
  }
  vi.stubGlobal('EventSource', Source)
  const business = vi.fn(), changed = vi.fn()
  const unsubscribeBusiness = subscribeEvents(['codex.message.delta'], business)
  const unsubscribeStatus = subscribeEventConnection(changed)
  try {
    expect(sources).toHaveLength(1)
    expect(readEventConnection()).toBe('connecting')
    sources[0].dispatchEvent(new Event('open'))
    expect(readEventConnection()).toBe('connected')
    sources[0].dispatchEvent(new MessageEvent('codex.message.delta', { data: '{}' }))
    expect(business).toHaveBeenCalledOnce()
    sources[0].dispatchEvent(new Event('error'))
    expect(readEventConnection()).toBe('reconnecting')
    sources[0].dispatchEvent(new Event('open'))
    expect(readEventConnection()).toBe('connected')
    expect(changed).toHaveBeenCalledTimes(3)
    unsubscribeStatus()
    expect(sources[0].closed).toBe(false)
    unsubscribeBusiness()
    expect(sources[0].closed).toBe(true)
    expect(readEventConnection()).toBe('connecting')
  } finally {
    unsubscribeStatus(); unsubscribeBusiness(); vi.unstubAllGlobals()
  }
})
