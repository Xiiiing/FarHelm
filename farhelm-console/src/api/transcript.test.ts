import { describe, expect, it } from 'vitest'
import type { TranscriptTurn } from './features'
import { mergeDeltas, mergeTurns, type TranscriptDelta } from './transcript'

const delta = (turn_id: string, item_id: string, text_offset: number, text: string): TranscriptDelta => ({ turn_id, item_id, text_offset, delta: text })
const serial = (current: TranscriptTurn[], chunks: TranscriptDelta[]) => chunks.reduce((turns, chunk) => mergeTurns(turns, [{
  turn_id: chunk.turn_id, status: 'inProgress', items: [{ item_id: chunk.item_id, kind: 'assistant_message', text: chunk.delta, text_offset: chunk.text_offset, text_complete: false, streaming: true }],
}]), current)

describe('batched transcript deltas', () => {
  it('preserves interleaved turn/item identity, Unicode gaps, overlaps and duplicate order', () => {
    const chunks = [
      delta('a', 'answer', 3, '完成'), delta('b', 'answer', 0, '独立'),
      delta('a', 'second', 0, '旁白'), delta('a', 'answer', 0, '你好🙂'),
      delta('b', 'answer', 2, '输出'), delta('a', 'answer', 3, '完成'),
      delta('a', 'answer', 2, '🙂完成！'), delta('a', 'answer', 8, '末'),
      delta('a', 'answer', 6, '补齐'),
    ]
    const result = mergeDeltas([], chunks)
    expect(result).toEqual(serial([], chunks))
    expect(result.map((turn) => turn.turn_id)).toEqual(['a', 'b'])
    expect(result[0].items.map((item) => item.text)).toEqual(['你好🙂完成！补齐末', '旁白'])
    expect(result[1].items[0].text).toBe('独立输出')
    for (let split = 0; split <= chunks.length; split++) expect(mergeDeltas(mergeDeltas([], chunks.slice(0, split)), chunks.slice(split))).toEqual(result)
  })

  it('does not mutate cached turns and retains unaffected turn/item references', () => {
    const current: TranscriptTurn[] = Array.from({ length: 2000 }, (_, index) => ({ turn_id: `t${index}`, status: 'inProgress', items: [
      { item_id: 'user', kind: 'user_message', text: '问题' }, { item_id: 'answer', kind: 'assistant_message', text: '开始🙂', text_offset: 0, text_complete: false },
    ] }))
    for (const turn of current) { turn.items.forEach(Object.freeze); Object.freeze(turn.items); Object.freeze(turn) }
    Object.freeze(current)
    const result = mergeDeltas(current, [delta('t1999', 'answer', 3, '完成'), delta('t1999', 'answer', 5, '！')])
    expect(result[1999].items[1].text).toBe('开始🙂完成！')
    expect(current[1999].items[1].text).toBe('开始🙂')
    expect(result[1999].items[0]).toBe(current[1999].items[0])
    expect(result[1999]).not.toBe(current[1999])
    for (let index = 0; index < 1999; index++) expect(result[index]).toBe(current[index])
  })

  it('keeps authoritative history/continuations and ignores late terminal deltas', () => {
    const streamed = mergeDeltas([], [delta('t', 'answer', 3, '结尾'), delta('t', 'answer', 0, '前缀🙂')])
    const bounded: TranscriptTurn[] = [{ turn_id: 't', status: 'completed', items: [{ item_id: 'answer', kind: 'assistant_message', text: '前缀🙂', text_offset: 0, text_complete: false }] }]
    const refreshed = mergeTurns(streamed, bounded)
    const completed = mergeTurns(refreshed, [{ turn_id: 't', status: 'completed', items: [{ item_id: 'answer', kind: 'assistant_message', text: '结尾', text_offset: 3, text_complete: true }] }], true)
    const late = [delta('t', 'answer', 5, '不能追加'), delta('t', 'other', 0, '不能创建')]
    expect(completed[0].items[0]).toMatchObject({ text: '前缀🙂结尾', text_complete: true })
    expect(mergeDeltas(completed, late)).toBe(completed)
    expect(mergeDeltas(completed, late)).toEqual(serial(completed, late))
    expect(mergeDeltas(completed, [])).toBe(completed)
  })

  it('retains a complete item while accepting another item in the same active turn', () => {
    const current: TranscriptTurn[] = [{ turn_id: 'active', status: 'inProgress', items: [{ item_id: 'done', kind: 'assistant_message', text: '已完成', text_complete: true }] }]
    const chunks = [delta('active', 'done', 3, '迟到'), delta('active', 'next', 0, ''), delta('active', 'next', 0, '继续🙂')]
    const result = mergeDeltas(current, chunks)
    expect(result).toEqual(serial(current, chunks))
    expect(result[0].items[0]).toBe(current[0].items[0])
    expect(result[0].items[1].text).toBe('继续🙂')
  })
})
