import { afterEach, describe, expect, it, vi } from 'vitest'
import { mutate, waitForCommand, type TranscriptTurn } from './features'
import { mergeTurns } from './transcript'
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
