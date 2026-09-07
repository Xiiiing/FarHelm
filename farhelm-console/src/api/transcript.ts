import type { TranscriptItem, TranscriptTurn } from './features'
type FragmentItem = TranscriptItem & { parts?: Record<number, string>; completeAt?: number }
const terminal = (status: string) => ['completed', 'failed', 'interrupted', 'orphaned'].includes(status)
function mergeItem(old: FragmentItem | undefined, incoming: TranscriptItem): FragmentItem {
  if (incoming.streaming && old?.text_complete && !old.streaming) return old
  const offset = incoming.text_offset ?? 0
  // A bounded first page is a prefix, not a replacement for continuation
  // fragments already read. Retain them only when the authoritative prefix agrees.
  const matchingPrefix = incoming.text_complete === false && old && !old.streaming && old.text.startsWith(incoming.text)
  const prior = !incoming.streaming && offset === 0 && !matchingPrefix ? undefined : old
  const parts = { ...(prior?.parts ?? (prior ? { [prior.text_offset ?? 0]: prior.text } : {})), [offset]: incoming.text }
  let text = ''; let end = 0
  for (const [start, value] of Object.entries(parts).sort(([a], [b]) => Number(a) - Number(b))) {
    const position = Number(start)
    if (position > end) break
    const suffix = position === end ? value : Array.from(value).slice(end - position).join('')
    text += suffix; end += Array.from(suffix).length
  }
  const completeAt = incoming.text_complete === true || (!incoming.streaming && incoming.text_complete === undefined) ? offset + Array.from(incoming.text).length : prior?.completeAt
  return { ...old, ...incoming, text, text_offset: 0, text_complete: completeAt !== undefined && end >= completeAt, completeAt, parts }
}
export function mergeTurns(current: TranscriptTurn[], incoming: TranscriptTurn[], older = false): TranscriptTurn[] {
  const merged = new Map(current.map((turn) => [turn.turn_id, turn]))
  for (const turn of incoming) {
    const previous = merged.get(turn.turn_id)
    const isDelta = turn.items.every((item) => item.streaming)
    if (isDelta && previous && terminal(previous.status)) continue
    const items = new Map(previous?.items.map((item) => [item.item_id, item]) ?? [])
    for (const item of turn.items) items.set(item.item_id, mergeItem(items.get(item.item_id), item))
    const order = !isDelta && !older ? [...turn.items, ...(previous?.items ?? [])] : [...(previous?.items ?? []), ...turn.items]
    merged.set(turn.turn_id, { ...previous, ...turn, items: [...new Set(order.map((item) => item.item_id))].map((id) => items.get(id)!) })
  }
  const order = older ? [...incoming, ...current] : [...current, ...incoming]
  return [...new Set(order.map((turn) => turn.turn_id))].map((id) => merged.get(id)!)
}

export function mergeDelta(current: TranscriptTurn[], data: { turn_id: string; item_id: string; delta: string; text_offset: number }): TranscriptTurn[] {
  return mergeTurns(current, [{ turn_id: data.turn_id, status: 'inProgress', items: [{ item_id: data.item_id, kind: 'assistant_message', text: data.delta, text_offset: data.text_offset, text_complete: false, streaming: true }] }])
}
