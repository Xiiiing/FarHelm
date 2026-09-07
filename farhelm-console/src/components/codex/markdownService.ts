import type { toJsxRuntime } from 'hast-util-to-jsx-runtime'
export type MarkdownTree = Parameters<typeof toJsxRuntime>[0]
type Job = { id: number; text: string; done: (tree?: MarkdownTree) => void; cancelled?: boolean }
let sequence = 0
let worker: Worker | undefined
let current: Job | undefined
let idle: ReturnType<typeof setTimeout> | undefined
const jobs: Job[] = []
const cache = new Map<string, { tree: MarkdownTree; bytes: number }>()
let cacheBytes = 0
export function cachedMarkdown(text: string) { return cache.get(text)?.tree }
export function clearMarkdownCache() { cache.clear(); cacheBytes = 0; worker?.terminate(); worker = undefined; current = undefined; jobs.splice(0); clearTimeout(idle) }
function retain(text: string, tree: MarkdownTree, bytes: number) {
  if (bytes > 8 * 1024 * 1024) return
  cacheBytes -= cache.get(text)?.bytes ?? 0; cache.delete(text); cache.set(text, { tree, bytes }); cacheBytes += bytes
  while (cache.size > 20 || cacheBytes > 8 * 1024 * 1024) { const key = cache.keys().next().value!; cacheBytes -= cache.get(key)!.bytes; cache.delete(key) }
}

function pump() {
  if (current) return
  if (idle) clearTimeout(idle)
  current = jobs.shift()
  if (!current) { idle = setTimeout(() => { worker?.terminate(); worker = undefined }, 5000); return }
  if (!worker) {
    try { worker = new Worker(new URL('./markdown.worker.ts', import.meta.url), { type: 'module' }) }
    catch { const job = current; current = undefined; if (!job.cancelled) job.done(); queueMicrotask(pump); return }
    worker.onmessage = (event: MessageEvent<{ id: number; tree?: MarkdownTree; bytes: number }>) => {
      if (current?.id !== event.data.id) return
      const job = current; current = undefined; if (event.data.tree) retain(job.text, event.data.tree, event.data.bytes); if (!job.cancelled) job.done(event.data.tree); pump()
    }
    worker.onerror = () => { const job = current; current = undefined; worker?.terminate(); worker = undefined; if (job && !job.cancelled) job.done(); pump() }
  }
  worker.postMessage({ id: current.id, text: current.text })
}

export function renderMarkdown(text: string, done: Job['done']): () => void {
  const cached = cachedMarkdown(text)
  if (cached) { done(cached); return () => {} }
  const id = ++sequence
  // Virtualization normally keeps this below ten. Bound pending parser work even
  // when a large response inserts many independent Markdown messages at once.
  if (jobs.length >= 32) { const old = jobs.shift()!; if (!old.cancelled) old.done() }
  jobs.push({ id, text, done }); pump()
  return () => {
    const index = jobs.findIndex((job) => job.id === id)
    if (index >= 0) jobs.splice(index, 1)
    if (current?.id === id) current.cancelled = true
  }
}
