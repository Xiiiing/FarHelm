// One shared SSE connection per authenticated browser page.
type Listener = (event: MessageEvent<string>) => void
const listeners = new Map<string, Set<Listener>>()
let stream: EventSource | undefined
const dispatchers = new Map<string, EventListener>()
export function subscribeEvents(names: string[], listener: Listener): () => void {
  stream ??= new EventSource('/api/v1/events/stream')
  for (const name of names) {
    if (!listeners.has(name)) {
      listeners.set(name, new Set())
      let queued: ReturnType<typeof setTimeout> | undefined
      let latest: Event
      const dispatch: EventListener = (event) => {
        if (name === 'codex.message.delta' || name === 'open') { listeners.get(name)?.forEach((fn) => fn(event as MessageEvent<string>)); return }
        latest = event
        queued ??= setTimeout(() => { queued = undefined; listeners.get(name)?.forEach((fn) => fn(latest as MessageEvent<string>)) }, 150)
      }
      dispatchers.set(name, dispatch)
      stream.addEventListener(name, dispatch)
    }
    listeners.get(name)!.add(listener)
  }
  return () => {
    for (const name of names) listeners.get(name)?.delete(listener)
    if ([...listeners.values()].every((set) => set.size === 0)) {
      stream?.close(); stream = undefined; listeners.clear(); dispatchers.clear()
    }
  }
}
