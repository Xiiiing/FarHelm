import type { TranscriptItem, TranscriptTurn } from './features'
type FragmentItem = TranscriptItem & { parts?: Record<number, string>; completeAt?: number; textEnd?: number }
const terminal = (status: string) => ['completed', 'failed', 'interrupted', 'orphaned'].includes(status)
function length(text: string) { let count = 0; for (let index = 0; index < text.length; count++) index += text.codePointAt(index)! > 0xffff ? 2 : 1; return count }
function suffix(text: string, offset: number) { let index = 0; for (const point of text) { if (offset-- <= 0) break; index += point.length } return text.slice(index) }
function mergeItem(old: FragmentItem | undefined, incoming: TranscriptItem): FragmentItem {
  if (incoming.streaming && old?.text_complete && !old.streaming) return old
  const offset = incoming.text_offset ?? 0
  const matchingPrefix = incoming.text_complete === false && old && !old.streaming && old.text.startsWith(incoming.text)
  const prior = !incoming.streaming && offset === 0 && !matchingPrefix ? undefined : old
  let text = prior?.text ?? ''; let end = prior?.textEnd ?? length(text)
  const parts = { ...prior?.parts }
  const append = (position: number, value: string) => {
    if (position > end) { parts[position] = value; return }
    const tail = position === end ? value : suffix(value, end - position)
    text += tail; end += length(tail)
  }
  append(offset, incoming.text)
  for (const start of Object.keys(parts).map(Number).sort((a, b) => a - b)) {
    if (start > end) break
    append(start, parts[start]); delete parts[start]
  }
  const completeAt = incoming.text_complete === true || (!incoming.streaming && incoming.text_complete === undefined) ? offset + length(incoming.text) : prior?.completeAt
  return { ...old, ...incoming, text, text_offset: 0, textEnd: end, text_complete: completeAt !== undefined && end >= completeAt, completeAt, parts }
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
