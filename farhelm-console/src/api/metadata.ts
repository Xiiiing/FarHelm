import { queryClient } from './cache'
import { subscribeEvents } from './events'

// Metadata pages share the authenticated SSE connection and retain their loaded pages.
export function connectMetadataCache() {
  const pending = new Set<string>()
  let timer: ReturnType<typeof setTimeout> | undefined
  let opened = false
  const queue = (...keys: string[]) => {
    keys.forEach(key => pending.add(key))
    timer ??= setTimeout(() => {
      timer = undefined
      for (const key of pending) {
        if (key.includes('/') && pending.has(key.split('/')[0])) continue
        void queryClient.invalidateQueries({ queryKey: key.split('/') })
      }
      pending.clear()
    }, 50)
  }
  const off = subscribeEvents(['open', 'notification.created', 'notification.changed', 'experiment.updated', 'experiment.reported', 'command.updated', 'project.updated'], event => {
    if (event.type === 'open') { if (opened) queue('notifications', 'experiment-runs', 'audit'); opened = true }
    else if (event.type === 'notification.created') queue('notifications/list', 'audit')
    else if (event.type === 'notification.changed') queue('notifications', 'audit')
    else if (event.type.startsWith('experiment.')) queue('experiment-runs', 'audit')
    else queue('audit')
  })
  return () => { off(); clearTimeout(timer) }
}

export function uniqueRows<T>(rows: T[], identity: (row: T) => string | number): T[] {
  const seen = new Set<string | number>()
  return rows.filter(row => { const key = identity(row); if (seen.has(key)) return false; seen.add(key); return true })
}
