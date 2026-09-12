import { QueryClient, notifyManager, type InfiniteData } from '@tanstack/react-query'
import type { CodexSession, DisplayPage, Operation, TranscriptPage } from './features'
import type { AgentListResponse, AgentSummary } from './agents'
import { mergeDeltas, mergeTurns, type TranscriptDelta } from './transcript'
import { subscribeEvents } from './events'

export const queryClient = new QueryClient({ defaultOptions: { queries: { staleTime: 30_000, gcTime: 5 * 60_000, retry: false, refetchOnWindowFocus: false, structuralSharing: false }, mutations: { retry: false } } })
export const keys = {
  session: (id: string) => ['codex', 'session', id] as const,
  history: (id: string) => ['codex', 'history', id] as const,
  operation: (id: string) => ['codex', 'operation', id] as const,
}
const terminal = (state?: string) => !!state && ['completed', 'failed', 'expired', 'orphaned', 'interrupted', 'unknown'].includes(state)
export function mergeSession(value: CodexSession): CodexSession {
  const old = queryClient.getQueryData<CodexSession>(keys.session(value.session_id))
  if (old && (old.revision ?? 0) > (value.revision ?? 0)) return old
  return { ...old, ...value, title: value.title || old?.title, active_turn_id: value.active_turn_id }
}
export function cacheOperation(id: string, value: Operation) {
  queryClient.setQueryData<Operation>(keys.operation(id), (old) => terminal(old?.state) && !terminal(value.state) ? old : { ...old, ...value, data: { ...old?.data, ...value.data } })
}
export function cacheHistory(id: string, page: TranscriptPage, older = false): TranscriptPage {
  const previous = queryClient.getQueryData<TranscriptPage>(keys.history(id))
  return { ...page, context: older ? previous?.context : page.context, ...(previous?.older_loaded && !older ? { next_cursor: previous.next_cursor, continuation: previous.continuation } : {}), older_loaded: older || previous?.older_loaded, turns: mergeTurns(previous?.turns ?? [], [...page.turns].reverse(), older) }
}

// Estimate retained strings without serializing megabyte bodies on the UI thread.
let pruning = false
let accessSequence = 0
const accessed = new WeakMap<object, number>()
queryClient.getQueryCache().subscribe((event) => {
  if (pruning || !['updated', 'observerAdded', 'observerRemoved'].includes(event.type) || event.query.queryKey[1] !== 'history') return
  accessed.set(event.query, ++accessSequence)
  pruning = true
  const inactive = queryClient.getQueryCache().findAll({ queryKey: ['codex', 'history'], type: 'inactive' }).sort((a, b) => (accessed.get(b) ?? 0) - (accessed.get(a) ?? 0))
  let bytes = 0
  inactive.forEach((query, index) => {
    const page = query.state.data as TranscriptPage | undefined
    bytes += page?.turns.reduce((sum, turn) => sum + turn.items.reduce((total, item) => total + item.text.length * 4 + 256, 0), 0) ?? 0
    if (index >= 20 || bytes > 24 * 1024 * 1024) queryClient.removeQueries({ queryKey: query.queryKey, exact: true })
  })
  pruning = false
})

function updateSession(payload: Partial<CodexSession> & { update_kind?: string }) {
  const id = payload.session_id
  // Durable replay contains raw Agent events, not the versioned live projection.
  // Recovery reconciles lists once; replay cannot overwrite or refetch per row.
  if (!id || !Number.isSafeInteger(payload.revision) || payload.revision! < 0) return
  const known = queryClient.getQueryData<CodexSession>(keys.session(id))
  if (known && (known.revision ?? 0) > (payload.revision ?? 0)) return
  const patch = (old?: CodexSession): CodexSession | undefined => {
    if (old && (old.revision ?? 0) > (payload.revision ?? 0)) return old
    if (!old && (!payload.agent_id || !payload.project_id || !payload.mode)) return old
    const value = { ...old, ...payload } as CodexSession
    if (!payload.title) value.title = old?.title
    if (['metadata', 'name'].includes(payload.update_kind ?? '') && old) { value.mode = old.mode; if (payload.update_kind === 'name' || ['creating', 'queued', 'running', 'interrupting'].includes(old.state)) value.state = old.state; else if (payload.state !== 'archived') value.state = old.state === 'archived' ? 'idle' : old.state; value.active_turn_id = old.active_turn_id }
    return value
  }
  queryClient.setQueryData<CodexSession>(keys.session(id), patch)
  for (const query of queryClient.getQueryCache().findAll({ queryKey: ['codex', 'sessions'] })) {
    const old = query.state.data as InfiniteData<DisplayPage> | undefined
    if (!old) continue
    const existing = old.pages.find((page) => page.sessions.some((row) => row.session_id === id))?.sessions.find((row) => row.session_id === id)
    if (existing && (existing.revision ?? 0) > (payload.revision ?? 0)) continue
    const value = patch(existing) ?? queryClient.getQueryData<CodexSession>(keys.session(id)) ?? payload
    const [, , archived, agent, project, search] = query.queryKey
    // Missing metadata is unknown, not evidence that the event belongs elsewhere.
    const outside = (agent && value.agent_id && agent !== value.agent_id) || (project && value.project_id && project !== value.project_id)
      || (value.state && ((archived === 'true' && value.state !== 'archived') || (archived === 'false' && value.state === 'archived')))
    if (!existing && outside) continue
    if (existing) {
      queryClient.setQueryData(query.queryKey, { ...old, pages: old.pages.map((page) => !page.sessions.some((row) => row.session_id === id) ? page : {
        ...page, sessions: outside ? page.sessions.filter((row) => row.session_id !== id) : page.sessions.map((row) => row.session_id === id ? value : row),
      }) })
    }
    // Search also depends on Agent-only preview text, so its membership needs a read.
    if (!existing || outside || search) void queryClient.invalidateQueries({ queryKey: query.queryKey, exact: true })
  }
}

/** One reducer updates every visible/cache consumer; components do not refetch on each event. */
export function connectCodexCache() {
  let opened = false
  const refresh = new Map<string, ReturnType<typeof setTimeout>>()
  const reconciled = new Set<string>()
  let frame: ReturnType<typeof setTimeout> | undefined
  let listingRefresh: ReturnType<typeof setTimeout> | undefined
  const deltas = new Map<string, TranscriptDelta[]>()
  const flush = () => {
    clearTimeout(frame); frame = undefined
    notifyManager.batch(() => {
      for (const [id, chunks] of deltas) queryClient.setQueryData<TranscriptPage>(keys.history(id), (old) => {
        const turns = mergeDeltas(old?.turns ?? [], chunks)
        return old?.turns === turns ? old : { session_id: id, ...old, turns }
      })
      deltas.clear()
    })
  }
  const refreshHistory = (id: string) => {
    if (refresh.has(id)) return
    refresh.set(id, setTimeout(() => { refresh.delete(id); void queryClient.invalidateQueries({ queryKey: keys.history(id), exact: true, refetchType: 'active' }) }, 50))
  }
  const reconcileLists = () => {
    listingRefresh ??= setTimeout(() => {
      listingRefresh = undefined
      // Also stale inactive scopes so archive/search switches do not reuse old membership.
      void queryClient.invalidateQueries({ queryKey: ['codex'], predicate: (query) => ['sessions', 'labels'].includes(String(query.queryKey[1])), refetchType: 'active' })
    }, 50)
  }
  const reconcileTerminal = (session: string, operation?: string, turn?: string) => {
    const identities = [...(operation ? [`operation:${operation}`] : []), ...(turn ? [`turn:${session}:${turn}`] : [])]
    const seen = identities.some((identity) => reconciled.has(identity))
    for (const identity of identities) reconciled.add(identity)
    while (reconciled.size > 512) reconciled.delete(reconciled.values().next().value!)
    if (!seen) refreshHistory(session)
  }
  const off = subscribeEvents(['open', 'codex.stream.resync', 'agent.status', 'project.discovered', 'project.updated', 'project.sync.updated', 'project.preferences.updated', 'command.updated', 'codex.schedule.updated', 'experiment.updated', 'experiment.reported', 'codex.session.updated', 'codex.turn.started', 'codex.turn.completed', 'codex.turn.failed', 'codex.turn.orphaned', 'codex.message.delta', 'codex.native.changed', 'codex.session.deleted'], (event) => {
    if (event.type === 'open' || event.type === 'codex.stream.resync') {
      if (opened || event.type === 'codex.stream.resync') {
        flush()
        reconcileLists()
        void queryClient.invalidateQueries({ queryKey: ['projects'] })
        void queryClient.invalidateQueries({ queryKey: ['project-preferences'] })
        for (const query of queryClient.getQueryCache().findAll({ queryKey: ['codex'], type: 'active' })) if (['history', 'session', 'operation', 'schedules', 'native'].includes(String(query.queryKey[1]))) void queryClient.invalidateQueries({ queryKey: query.queryKey, exact: true })
      }
      opened = true; return
    }
    try {
      const { payload } = JSON.parse(event.data) as { payload: Partial<CodexSession> & Operation & { update_kind?: string; operation_id?: string; data?: { turn_id?: string; item_id?: string; delta?: string; text_offset?: number; session_id?: string } } }
      if (event.type.startsWith('project.')) { if (event.type === 'project.preferences.updated') { void queryClient.invalidateQueries({ queryKey: ['project-preferences'] }); reconcileLists() } else void queryClient.invalidateQueries({ queryKey: ['projects'] }) }
      else if (event.type.startsWith('experiment.')) { void queryClient.invalidateQueries({ queryKey: ['experiments'] }) }
      else if (event.type === 'codex.schedule.updated') { void queryClient.invalidateQueries({ queryKey: ['codex', 'schedules', payload.session_id] }) }
      else if (event.type === 'agent.status') {
        const agent = payload as unknown as AgentSummary
        queryClient.setQueryData<AgentListResponse>(['agents'], (old) => ({ protocol: 'farhelm/1', agents: [...(old?.agents.filter((a) => a.agent_id !== agent.agent_id) ?? []), agent] }))
        if (agent.online) for (const query of queryClient.getQueryCache().findAll({ queryKey: ['codex', 'history'], type: 'active' })) if (query.state.status === 'error') refreshHistory(String(query.queryKey[2]))
      } else if (event.type === 'command.updated' && payload.command_id) {
        cacheOperation(payload.command_id, payload)
        if (terminal(payload.state) && payload.data?.session_id) reconcileTerminal(payload.data.session_id, payload.command_id, payload.data.turn_id)
      } else if (event.type === 'codex.session.updated') {
        updateSession(payload)
        if (payload.session_id && queryClient.getQueryState(keys.history(payload.session_id))?.status === 'error') refreshHistory(payload.session_id)
      }
      else if (event.type === 'codex.session.deleted' && payload.session_id) {
        queryClient.removeQueries({ queryKey: keys.history(payload.session_id), exact: true })
        void queryClient.invalidateQueries({ queryKey: keys.session(payload.session_id), exact: true })
        reconcileLists()
      }
      else if (event.type === 'codex.native.changed' && payload.session_id) {
        flush()
        refreshHistory(payload.session_id)
        void queryClient.invalidateQueries({ queryKey: ['codex', 'native', payload.session_id] })
        void queryClient.invalidateQueries({ queryKey: keys.session(payload.session_id) })
        reconcileLists()
      }
      else if (payload.session_id) {
        const id = payload.session_id; const data = payload.data
        if (event.type === 'codex.message.delta' && data?.turn_id && data.item_id && typeof data.delta === 'string' && Number.isSafeInteger(data.text_offset) && data.text_offset! >= 0) {
          const chunks = deltas.get(id) ?? []
          chunks.push({ turn_id: data.turn_id, item_id: data.item_id, delta: data.delta, text_offset: data.text_offset! })
          deltas.set(id, chunks)
          frame ??= setTimeout(flush, 24)
        } else {
          flush()
          const state = event.type === 'codex.turn.started' ? 'running' : event.type.split('.').at(-1)!
          if (payload.operation_id) cacheOperation(payload.operation_id, { command_id: payload.operation_id, state, data: { turn_id: data?.turn_id, session_id: id } })
          const context = queryClient.getQueryData<TranscriptPage>(keys.history(id))?.context
          // A first resume confirms the previously unknown native permission profile.
          if (state === 'running' && context && !context.sandbox) refreshHistory(id)
          if (terminal(state)) {
            queryClient.setQueryData<TranscriptPage>(keys.history(id), (old) => old && ({ ...old, turns: old.turns.map((turn) => turn.turn_id === data?.turn_id ? { ...turn, status: state } : turn) }))
            reconcileTerminal(id, payload.operation_id, data?.turn_id)
          }
        }
      }
    } catch { /* Unknown frames never create anonymous transcript content. */ }
  })
  return () => { off(); clearTimeout(frame); clearTimeout(listingRefresh); for (const timer of refresh.values()) clearTimeout(timer); deltas.clear() }
}
