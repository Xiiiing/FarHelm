import { afterEach, describe, expect, it } from 'vitest'
import { cacheHistory, cacheOperation, keys, mergeSession, queryClient } from './cache'
import type { CodexSession, TranscriptPage } from './features'

afterEach(() => queryClient.clear())
const page = (id: string, text = '正文'): TranscriptPage => ({ session_id: id, turns: [{ turn_id: 't', status: 'completed', items: [{ item_id: 'i', kind: 'assistant_message', text }] }] })
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
})
