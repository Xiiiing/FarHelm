// One shared SSE connection per authenticated browser page.
type Listener = (event: MessageEvent<string>) => void
const listeners = new Map<string, Set<Listener>>()
let stream: EventSource | undefined
const dispatchers = new Map<string, EventListener>()
let connectionState: 'connecting' | 'connected' | 'reconnecting' = 'connecting'
export function readEventConnection() { return connectionState }
export function subscribeEventConnection(listener: () => void) { return subscribeEvents(['open', 'error'], listener) }
export function subscribeEvents(names: string[], listener: Listener): () => void {
  if (!stream) {
    connectionState = 'connecting'
    stream = new EventSource('/api/v1/events/stream')
    stream.addEventListener('open', () => { connectionState = 'connected' })
    stream.addEventListener('error', () => { connectionState = 'reconnecting' })
  }
  for (const name of names) {
    if (!listeners.has(name)) {
      listeners.set(name, new Set())
      const dispatch: EventListener = (event) => {
        // Preserve every identity and event order. Consumers coalesce their own
        // refreshes; dropping all but the last event loses other sessions' turns.
        listeners.get(name)?.forEach((fn) => fn(event as MessageEvent<string>))
      }
      dispatchers.set(name, dispatch)
      stream.addEventListener(name, dispatch)
    }
    listeners.get(name)!.add(listener)
  }
  return () => {
    for (const name of names) listeners.get(name)?.delete(listener)
    if ([...listeners.values()].every((set) => set.size === 0)) {
      stream?.close(); stream = undefined; connectionState = 'connecting'; listeners.clear(); dispatchers.clear()
    }
  }
}
