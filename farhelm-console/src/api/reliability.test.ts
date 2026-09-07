import { afterEach, describe, expect, it, vi } from 'vitest'
import { mutate, waitForCommand, type TranscriptTurn } from './features'
import { mergeDelta, mergeTurns } from './transcript'
import { normalizeMath } from '../components/codex/math'
import { sessionName } from '../components/codex/presentation'
import { subscribeEvents } from './events'

afterEach(() => vi.unstubAllGlobals())

describe('submission identity', () => {
  it('keeps the same operation identity when the save response is lost', async () => {
    const request = vi.fn().mockRejectedValueOnce(new Error('connection lost')).mockResolvedValue({ ok: true, status: 202, json: async () => ({ command_id: 'saved' }) })
    vi.stubGlobal('fetch', request)
    await expect(mutate('/retry', 'csrf', { prompt: 'check' })).rejects.toThrow('connection lost')
    await mutate('/retry', 'csrf', { prompt: 'check' })
    expect(request.mock.calls[0][1].headers['Idempotency-Key']).toBe(request.mock.calls[1][1].headers['Idempotency-Key'])
    await mutate('/retry', 'csrf', { prompt: 'check' })
    expect(request.mock.calls[2][1].headers['Idempotency-Key']).not.toBe(request.mock.calls[1][1].headers['Idempotency-Key'])
  })
  it('reuses the saved receipt after status polling fails', async () => {
    const request = vi.fn().mockResolvedValueOnce({ ok: true, status: 202, json: async () => ({ command_id: 'scheduled' }) }).mockRejectedValueOnce(new Error('offline')).mockResolvedValue({ ok: true, status: 202, json: async () => ({ command_id: 'scheduled' }) })
    vi.stubGlobal('fetch', request)
    const receipt = await mutate('/schedule', 'csrf', { prompt: 'check' })
    await expect(waitForCommand(receipt)).rejects.toThrow('offline')
    await mutate('/schedule', 'csrf', { prompt: 'check' })
    expect(request.mock.calls[0][1].headers['Idempotency-Key']).toBe(request.mock.calls[2][1].headers['Idempotency-Key'])
  })
})

it('merges repeated Unicode fragments by turn, item and code-point offset', () => {
  const turn = (text: string, offset: number, complete: boolean): TranscriptTurn => ({ turn_id: 't', status: 'completed', items: [{ item_id: 'i', kind: 'assistant_message', text, text_offset: offset, text_complete: complete }] })
  let result = mergeTurns([], [turn('开始🙂', 0, false)])
  result = mergeTurns(result, [turn('结果', 3, true)], true)
  result = mergeTurns(result, [turn('结果', 3, true)], true)
  expect(result[0].items[0].text).toBe('开始🙂结果')
  result = mergeTurns(result, [turn('完整🙂新结果', 0, true)])
  expect(result[0].items[0].text).toBe('完整🙂新结果')
})

it('shares one stream and closes it after the last subscriber', () => {
  let count = 0; const close = vi.fn()
  vi.stubGlobal('EventSource', class extends EventTarget { constructor() { super(); count++ } close = close })
  const a = subscribeEvents(['open'], () => {})
  const b = subscribeEvents(['codex.message.delta'], () => {})
  expect(count).toBe(1); a(); expect(close).not.toHaveBeenCalled(); b(); expect(close).toHaveBeenCalledOnce()
})

it('keeps out-of-order offsets incomplete until gaps arrive, and deduplicates authoritative items', () => {
  let turns: TranscriptTurn[] = []
  turns = mergeDelta(turns, { turn_id: 't', item_id: 'a', text_offset: 3, delta: '完成' })
  expect(turns[0].items[0].text).toBe('')
  turns = mergeDelta(turns, { turn_id: 't', item_id: 'b', text_offset: 0, delta: '另一个输出' })
  turns = mergeDelta(turns, { turn_id: 't', item_id: 'a', text_offset: 0, delta: '你好🙂' })
  expect(turns[0].items.map((i) => i.text)).toEqual(['你好🙂完成', '另一个输出'])
  turns = mergeTurns(turns, [{ turn_id: 't', status: 'completed', items: [{ item_id: 'a', kind: 'assistant_message', text: '你好🙂完成' }, { item_id: 'b', kind: 'assistant_message', text: '另一个输出' }] }])
  turns = mergeDelta(turns, { turn_id: 't', item_id: 'a', text_offset: 3, delta: '完成' })
  expect(turns[0].items).toHaveLength(2)
  expect(turns[0].items[0].text).toBe('你好🙂完成')
})

it('retains item order when continuing later items in the same turn', () => {
  const current: TranscriptTurn[] = [{ turn_id: 't', status: 'completed', items: [{ item_id: 'u', kind: 'user_message', text: 'question' }, { item_id: 'a', kind: 'assistant_message', text: 'first', text_complete: false, text_offset: 0 }] }]
  const continuation: TranscriptTurn[] = [{ turn_id: 't', status: 'completed', items: [{ item_id: 'a', kind: 'assistant_message', text: 'second', text_offset: 5, text_complete: true }, { item_id: 'tool', kind: 'command_summary', text: 'done' }] }]
  const result = mergeTurns(current, continuation, true)
  expect(result[0].items.map((item) => item.item_id)).toEqual(['u', 'a', 'tool'])
  expect(result[0].items[1].text).toBe('firstsecond')
})

it('retains a completed continuation across bounded refreshes but replaces a changed prefix', () => {
  const item = { item_id: 'a', kind: 'assistant_message' as const, text: '前缀🙂', text_offset: 0, text_complete: false }
  let turns = mergeTurns([], [{ turn_id: 't', status: 'completed', items: [item] }])
  turns = mergeTurns(turns, [{ turn_id: 't', status: 'completed', items: [{ ...item, text: '结尾', text_offset: 3, text_complete: true }] }], true)
  turns = mergeTurns(turns, [{ turn_id: 't', status: 'completed', items: [item] }])
  expect(turns[0].items[0]).toMatchObject({ text: '前缀🙂结尾', text_complete: true })
  turns = mergeTurns(turns, [{ turn_id: 't', status: 'completed', items: [{ ...item, text: '已修改' }] }])
  expect(turns[0].items[0]).toMatchObject({ text: '已修改', text_complete: false })
})

it('does not coalesce distinct session events out of existence', () => {
  const sources: EventTarget[] = []
  vi.stubGlobal('EventSource', class extends EventTarget { constructor() { super(); sources.push(this) } close() {} })
  const seen: string[] = []
  const off = subscribeEvents(['codex.turn.completed'], (event) => seen.push(event.data))
  sources[0].dispatchEvent(new MessageEvent('codex.turn.completed', { data: 'a' }))
  sources[0].dispatchEvent(new MessageEvent('codex.turn.completed', { data: 'b' }))
  expect(seen).toEqual(['a', 'b']); off()
})

it('converts both TeX delimiters without rewriting literal code', () => {
  expect(normalizeMath('内联 \\(x^2\\)，块 \\[x+y\\]')).toBe('内联 $x^2$，块 \n$$\nx+y\n$$\n')
  const code = '```python\ntext = "\\(literal\\)"\n```\n`\\[literal\\]`'
  expect(normalizeMath(code)).toBe(code)
})

it('uses ephemeral labels instead of placeholders and retains formal names', () => {
  const session = { session_id: 'session-long-id', project_id: 'cc08', agent_id: 'a', title: 'Codex session', display_label: '首条用户摘要', mode: 'inspect' as const, state: 'idle' as const, updated_at_unix: 100 }
  expect(sessionName(session)).toBe('首条用户摘要')
  expect(sessionName({ ...session, title: '正式标题' })).toBe('正式标题')
  expect(sessionName({ ...session, display_label: undefined })).toContain('cc08')
})
