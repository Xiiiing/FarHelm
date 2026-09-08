import { afterEach, describe, expect, it, vi } from 'vitest'
import { QueryObserver, type InfiniteData } from '@tanstack/react-query'
import { cacheHistory, cacheOperation, connectCodexCache, keys, mergeSession, queryClient } from './cache'
import type { CodexSession, DisplayPage, TranscriptPage } from './features'

const cleanup: (() => void)[] = []
afterEach(() => { cleanup.splice(0).forEach((stop) => stop()); queryClient.clear(); vi.useRealTimers(); vi.unstubAllGlobals() })
const page = (id: string, text = '正文'): TranscriptPage => ({ session_id: id, turns: [{ turn_id: 't', status: 'completed', items: [{ item_id: 'i', kind: 'assistant_message', text }] }] })
const session = (id: string, changes: Partial<CodexSession> = {}): CodexSession => ({ session_id: id, agent_id: 'a', project_id: 'p', mode: 'edit', state: 'idle', revision: 1, updated_at_unix: 1, ...changes })
const listKey = (agent = 'a', project = 'p', archived = 'false', search = '') => ['codex', 'sessions', archived, agent, project, search]
const listing = (...sessions: CodexSession[]): InfiniteData<DisplayPage> => ({ pages: [{ sessions, incomplete_agents: [] }], pageParams: [undefined] })
function stream() {
  const sources: EventTarget[] = []
  vi.stubGlobal('EventSource', class extends EventTarget { constructor() { super(); sources.push(this) } close() {} })
  cleanup.push(connectCodexCache())
  return (type: string, payload: object, lastEventId = '') => sources[0].dispatchEvent(new MessageEvent(type, { data: JSON.stringify({ event_id: `event-${lastEventId}`, payload }), lastEventId }))
}
describe('shared workspace cache', () => {
  it('retains the newest 20 inactive histories even when writes share a timestamp', () => {
    for (let i = 0; i < 25; i++) queryClient.setQueryData(keys.history(String(i)), page(String(i)))
    expect(queryClient.getQueryData(keys.history('0'))).toBeUndefined()
    expect(queryClient.getQueryData(keys.history('24'))).toBeDefined()
    expect(queryClient.getQueryCache().findAll({ queryKey: ['codex', 'history'] })).toHaveLength(20)
    queryClient.setQueryData(keys.history('large'), page('large', '字'.repeat(7 * 1024 * 1024)))
    expect(queryClient.getQueryData(keys.history('large'))).toBeUndefined()
  })
  it('keeps loaded older pages through a latest-page refresh and avoids duplicate turns', () => {
    queryClient.setQueryData(keys.history('s'), { ...page('s'), older_loaded: true, next_cursor: 'older-cursor' })
    const merged = cacheHistory('s', { ...page('s', '更新'), next_cursor: 'latest-cursor' })
    expect(merged.next_cursor).toBe('older-cursor'); expect(merged.turns).toHaveLength(1)
    expect(merged.turns[0].items[0].text).toBe('更新')
  })
  it('rejects a late snapshot and a late nonterminal receipt', () => {
    const current: CodexSession = { session_id: 's', agent_id: 'a', project_id: 'p', mode: 'edit', state: 'running', active_turn_id: 't', title: '正式名称', revision: 9, updated_at_unix: 1 }
    queryClient.setQueryData(keys.session('s'), current)
    expect(mergeSession({ ...current, state: 'idle', title: '', revision: 8 })).toEqual(current)
    expect(mergeSession({ ...current, state: 'idle', active_turn_id: undefined, revision: 10 }).active_turn_id).toBeUndefined()
    cacheOperation('op', { command_id: 'op', state: 'completed' }); cacheOperation('op', { command_id: 'op', state: 'accepted' })
    expect(queryClient.getQueryData(keys.operation('op'))).toMatchObject({ state: 'completed' })
  })

  it('does not refetch a live session list for unrelated Agent/project events', () => {
    const key = listKey(), data = listing(session('visible'))
    queryClient.setQueryData(key, data)
    const fetch = vi.fn(async () => data)
    cleanup.push(new QueryObserver(queryClient, { queryKey: key, queryFn: fetch }).subscribe(() => {}))
    const emit = stream()
    for (let index = 0; index < 20; index++) emit('codex.session.updated', session(`outside-${index}`, { agent_id: 'b', project_id: 'other' }))
    expect(fetch).not.toHaveBeenCalled()
    expect(queryClient.getQueryData(key)).toBe(data)
  })

  it('removes sessions that leave a scope and reconciles both destination and source', () => {
    const source = listKey(), target = listKey('a', 'other'), archived = listKey('a', 'other', 'true')
    queryClient.setQueryData(source, listing(session('moving')))
    queryClient.setQueryData(target, listing())
    queryClient.setQueryData(archived, listing())
    const emit = stream()
    emit('codex.session.updated', session('moving', { project_id: 'other', revision: 2 }))
    expect(queryClient.getQueryData<InfiniteData<DisplayPage>>(source)?.pages[0].sessions).toEqual([])
    expect(queryClient.getQueryState(source)?.isInvalidated).toBe(true)
    expect(queryClient.getQueryState(target)?.isInvalidated).toBe(true)
    expect(queryClient.getQueryState(archived)?.isInvalidated).toBe(false)
    queryClient.setQueryData(target, listing(session('moving', { project_id: 'other', revision: 2 })))
    emit('codex.session.updated', { session_id: 'moving', state: 'archived', revision: 3 })
    expect(queryClient.getQueryData<InfiniteData<DisplayPage>>(target)?.pages[0].sessions).toEqual([])
    expect(queryClient.getQueryState(archived)?.isInvalidated).toBe(true)
  })

  it('keeps reconciliation for unknown scope and search preview membership', () => {
    const ordinary = listKey(), search = listKey('a', 'p', 'false', '训练')
    queryClient.setQueryData(ordinary, listing(session('known')))
    queryClient.setQueryData(search, listing(session('known')))
    const emit = stream()
    emit('codex.session.updated', { session_id: 'unknown', state: 'running', revision: 2 })
    expect(queryClient.getQueryState(ordinary)?.isInvalidated).toBe(true)
    queryClient.setQueryData(ordinary, listing(session('known')))
    queryClient.setQueryData(search, listing(session('known')))
    emit('codex.session.updated', { session_id: 'known', title: '新名称', revision: 2 })
    expect(queryClient.getQueryState(ordinary)?.isInvalidated).toBe(false)
    expect(queryClient.getQueryState(search)?.isInvalidated).toBe(true)
  })

  it('keeps a migrated row out of its old scope when the reconciliation fails', async () => {
    const key = listKey()
    queryClient.setQueryData(key, listing(session('moving')))
    cleanup.push(new QueryObserver(queryClient, { queryKey: key, queryFn: async () => { throw new Error('offline') } }).subscribe(() => {}))
    const emit = stream()
    emit('codex.session.updated', session('moving', { state: 'archived', revision: 2 }))
    await vi.waitFor(() => expect(queryClient.getQueryState(key)?.status).toBe('error'))
    expect(queryClient.getQueryData<InfiniteData<DisplayPage>>(key)?.pages[0].sessions).toEqual([])
  })

  it('preserves metadata mode/state and rejects stale scope changes', () => {
    const key = listKey('a', 'p', 'true'), current = session('known', { state: 'archived', revision: 5 })
    queryClient.setQueryData(keys.session('known'), current)
    queryClient.setQueryData(key, listing(current))
    const emit = stream()
    emit('codex.session.updated', { ...current, project_id: 'elsewhere', state: 'idle', revision: 4 })
    expect(queryClient.getQueryState(key)?.isInvalidated).toBe(false)
    emit('codex.session.updated', { session_id: 'known', title: '正式名称', mode: 'inspect', state: 'idle', update_kind: 'metadata', revision: 6 })
    expect(queryClient.getQueryData<InfiniteData<DisplayPage>>(key)?.pages[0].sessions[0]).toMatchObject({ state: 'archived', mode: 'edit', title: '正式名称' })
    expect(queryClient.getQueryState(key)?.isInvalidated).toBe(false)
  })

  it('flushes all queued fragments before a terminal event and rejects later text', () => {
    vi.useFakeTimers()
    const emit = stream(), data = (item_id: string, text_offset: number, delta: string) => ({ session_id: 's', data: { turn_id: 't', item_id, text_offset, delta } })
    emit('codex.message.delta', data('a', 3, '完成'))
    emit('codex.message.delta', data('b', 0, '旁白'))
    emit('codex.message.delta', data('a', 0, '你好🙂'))
    expect(queryClient.getQueryData(keys.history('s'))).toBeUndefined()
    emit('codex.turn.completed', { session_id: 's', data: { turn_id: 't' } })
    const saved = queryClient.getQueryData<TranscriptPage>(keys.history('s'))!
    expect(saved.turns[0].status).toBe('completed')
    expect(saved.turns[0].items.map((item) => item.text)).toEqual(['你好🙂完成', '旁白'])
    emit('codex.message.delta', data('a', 5, '迟到'))
    vi.advanceTimersByTime(24)
    expect(queryClient.getQueryData(keys.history('s'))).toBe(saved)
  })

  it('publishes a received delta batch once without losing any text', () => {
    vi.useFakeTimers()
    const emit = stream(), published = vi.fn()
    cleanup.push(queryClient.getQueryCache().subscribe((event) => { if (event.type === 'updated' && event.query.queryKey[1] === 'history') published() }))
    for (let index = 0; index < 100; index++) emit('codex.message.delta', { session_id: 's', data: { turn_id: 't', item_id: 'i', text_offset: index, delta: '字' } })
    expect(published).not.toHaveBeenCalled()
    vi.advanceTimersByTime(24)
    expect(published).toHaveBeenCalledOnce()
    expect(queryClient.getQueryData<TranscriptPage>(keys.history('s'))?.turns[0].items[0].text).toBe('字'.repeat(100))
  })

  it.each(['reconnect', 'resync'])('reconciles archive membership once per %s without trusting raw replay', async (recovery) => {
    vi.useFakeTimers()
    const key = listKey(), archived = listKey('a', 'p', 'true'), labels = ['codex', 'labels', ['known']]
    const original = session('known', { title: '正式名称', revision: 5 })
    queryClient.setQueryData(keys.session('known'), original)
    queryClient.setQueryData(key, listing(original))
    queryClient.setQueryData(archived, listing())
    queryClient.setQueryData(labels, { sessions: [original], incomplete_agents: [] })
    let serverState: 'idle' | 'archived' = 'idle', revision = 5
    const fetch = vi.fn(async () => {
      const data = listing(...(serverState === 'archived' ? [] : [session('known', { title: '正式名称', revision })]))
      for (const row of data.pages[0].sessions) queryClient.setQueryData(keys.session(row.session_id), mergeSession(row))
      return data
    })
    const fetchLabels = vi.fn(async () => ({ sessions: [session('known', { title: '正式名称', revision, state: serverState })], incomplete_agents: [] }))
    cleanup.push(new QueryObserver(queryClient, { queryKey: key, queryFn: fetch }).subscribe(() => {}))
    cleanup.push(new QueryObserver(queryClient, { queryKey: labels, queryFn: fetchLabels }).subscribe(() => {}))
    const emit = stream()
    emit('open', {})
    // Matches persisted Hub payload: event id is not the session projection revision.
    const replay = (state: 'idle' | 'archived') => {
      for (let index = 0; index < 20; index++) emit('codex.session.updated', {
        update_kind: 'metadata', session_id: index % 2 ? 'unloaded' : 'known', project_id: 'p', mode: 'inspect', state, title: null, active_turn_id: null, updated_at_unix: 1,
      }, String(10_000 + index))
    }
    replay('archived')
    await vi.advanceTimersByTimeAsync(50)
    expect(fetch).not.toHaveBeenCalled()
    expect(queryClient.getQueryData(keys.session('known'))).toBe(original)
    expect(queryClient.getQueryData(keys.session('unloaded'))).toBeUndefined()
    for (const state of ['archived', 'idle'] as const) {
      serverState = state; revision++
      if (recovery === 'reconnect') { emit('error', {}); emit('open', {}) }
      else emit('codex.stream.resync', {})
      // A reconnect and resync may describe the same recovery boundary.
      emit('codex.stream.resync', {})
      replay(state)
      const previousCalls = fetch.mock.calls.length
      await vi.advanceTimersByTimeAsync(50)
      expect(fetch).toHaveBeenCalledTimes(previousCalls + 1)
      expect(fetchLabels).toHaveBeenCalledTimes(previousCalls + 1)
      expect(queryClient.getQueryData<InfiniteData<DisplayPage>>(key)?.pages[0].sessions.map((row) => row.session_id)).toEqual(state === 'archived' ? [] : ['known'])
      expect(queryClient.getQueryState(archived)?.isInvalidated).toBe(true)
      // Late raw replay does not create another read after the recovery batch.
      replay(state)
      await vi.advanceTimersByTimeAsync(100)
      expect(fetch).toHaveBeenCalledTimes(previousCalls + 1)
    }
    expect(queryClient.getQueryData(keys.session('known'))).toMatchObject({ state: 'idle', title: '正式名称', revision: 7 })
    emit('codex.session.updated', session('known', { state: 'archived', revision: 6 }))
    expect(queryClient.getQueryData(keys.session('known'))).toMatchObject({ state: 'idle', revision: 7 })
    expect(fetch).toHaveBeenCalledTimes(2)
  })

  it('retains visible cached membership when recovery cannot read the authoritative list', async () => {
    vi.useFakeTimers()
    const key = listKey(), original = session('known', { title: '正式名称', revision: 5 }), data = listing(original)
    queryClient.setQueryData(key, data)
    queryClient.setQueryData(keys.session('known'), original)
    const fetch = vi.fn(async () => { throw new Error('Agent offline') })
    cleanup.push(new QueryObserver(queryClient, { queryKey: key, queryFn: fetch }).subscribe(() => {}))
    const emit = stream()
    emit('open', {}); emit('error', {}); emit('open', {})
    emit('codex.session.updated', { session_id: 'known', project_id: 'p', mode: 'inspect', state: 'archived', title: null }, '99999')
    await vi.advanceTimersByTimeAsync(50)
    expect(fetch).toHaveBeenCalledOnce()
    expect(queryClient.getQueryState(key)?.status).toBe('error')
    expect(queryClient.getQueryData(key)).toBe(data)
    expect(queryClient.getQueryData(keys.session('known'))).toBe(original)
  })
})
