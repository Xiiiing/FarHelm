import type { TranscriptItem, TranscriptTurn } from './features'
type FragmentItem = TranscriptItem & { parts?: Record<number, string> }
function mergeItem(old: FragmentItem | undefined, incoming: TranscriptItem): FragmentItem {
  const prior = incoming.text_offset === 0 && incoming.text_complete ? undefined : old
  const parts = { ...(prior?.parts ?? (prior ? { [prior.text_offset ?? 0]: prior.text } : {})), [incoming.text_offset ?? 0]: incoming.text }
  let text = ''; let end = 0
  for (const [start, value] of Object.entries(parts).sort(([a], [b]) => Number(a) - Number(b))) {
    const offset = Number(start)
    if (offset > end) break
    const suffix = Array.from(value).slice(Math.max(0, end - offset)); text += suffix.join(''); end += suffix.length
  }
  return { ...old, ...incoming, text, text_offset: 0, text_complete: Boolean(old?.text_complete || incoming.text_complete), parts }
}
export function mergeTurns(current: TranscriptTurn[], incoming: TranscriptTurn[], older = false): TranscriptTurn[] {
  const merged = new Map(current.map((turn) => [turn.turn_id, turn]))
  for (const turn of incoming) {
    const previous = merged.get(turn.turn_id)
    const items = new Map(previous?.items.map((item) => [item.item_id, item]) ?? [])
    for (const item of turn.items) items.set(item.item_id, mergeItem(items.get(item.item_id), item))
    merged.set(turn.turn_id, { ...previous, ...turn, items: [...items.values()] })
  }
  const order = older ? [...incoming, ...current] : [...current, ...incoming]
  return [...new Set(order.map((turn) => turn.turn_id))].map((id) => merged.get(id)!)
}
