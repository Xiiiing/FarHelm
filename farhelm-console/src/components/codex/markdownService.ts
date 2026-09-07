import type { toJsxRuntime } from 'hast-util-to-jsx-runtime'
export type MarkdownTree = Parameters<typeof toJsxRuntime>[0]
type Job = { id: number; text: string; done: (tree?: MarkdownTree) => void }
let sequence = 0
let worker: Worker | undefined
let current: Job | undefined
let idle: ReturnType<typeof setTimeout> | undefined
const jobs: Job[] = []

function pump() {
  if (current) return
  if (idle) clearTimeout(idle)
  current = jobs.shift()
  if (!current) { idle = setTimeout(() => { worker?.terminate(); worker = undefined }, 5000); return }
  if (!worker) {
    worker = new Worker(new URL('./markdown.worker.ts', import.meta.url), { type: 'module' })
    worker.onmessage = (event: MessageEvent<{ id: number; tree?: MarkdownTree }>) => {
      if (current?.id !== event.data.id) return
      const job = current; current = undefined; job.done(event.data.tree); pump()
    }
    worker.onerror = () => { const job = current; current = undefined; worker?.terminate(); worker = undefined; job?.done(); pump() }
  }
  worker.postMessage({ id: current.id, text: current.text })
}

export function renderMarkdown(text: string, done: Job['done']): () => void {
  const id = ++sequence
  jobs.push({ id, text, done }); pump()
  return () => {
    const index = jobs.findIndex((job) => job.id === id)
    if (index >= 0) jobs.splice(index, 1)
    if (current?.id === id) { worker?.terminate(); worker = undefined; current = undefined; pump() }
  }
}
