import { useCallback, useEffect, useRef, useState } from 'react'
import { ApiError, json, sendMessage, type Operation } from '../../api/features'
import { subscribeEvents } from '../../api/events'
import { errorText } from './presentation'

export type PendingMessage = { id: string; text: string; state: string; command_id?: string; turn_id?: string; visible_turn_id?: string; delivery: 'queue' | 'steer' }
export type SessionDraft = { text: string; error?: string; sending: boolean; delivery: 'queue' | 'steer'; pending: PendingMessage[] }
const empty: SessionDraft = { text: '', sending: false, delivery: 'queue', pending: [] }
export function useOperations(csrf: string) {
  const [drafts, setDrafts] = useState<Record<string, SessionDraft>>({})
  const values = useRef(drafts); const locks = useRef(new Set<string>()); const turns = useRef(new Map<string, string>())
  const update = useCallback((session: string, change: (old: SessionDraft) => SessionDraft) => {
    const next = { ...values.current, [session]: change(values.current[session] ?? empty) }
    values.current = next; setDrafts(next)
  }, [])
  useEffect(() => subscribeEvents(['codex.turn.started', 'codex.turn.completed', 'codex.turn.failed', 'codex.turn.orphaned'], (event) => {
    try {
      const { payload } = JSON.parse(event.data) as { payload: { operation_id?: string; session_id?: string; data?: { turn_id?: string } } }
      if (!payload.operation_id || !payload.session_id || !payload.data?.turn_id) return
      const turnId = payload.data.turn_id
      turns.current.set(payload.operation_id, turnId)
      if (turns.current.size > 256) turns.current.delete(turns.current.keys().next().value!)
      update(payload.session_id, (old) => ({ ...old, pending: old.pending.map((p) => p.command_id === payload.operation_id ? { ...p, turn_id: turnId, state: event.type === 'codex.turn.started' ? 'running' : event.type.split('.').at(-1)! } : p) }))
    } catch { /* Malformed events are repaired by authoritative reads. */ }
  }), [update])
  useEffect(() => {
    let active = true; let busy = false
    const timer = setInterval(() => {
      if (busy) return
      const pending = Object.entries(values.current).flatMap(([session, draft]) => draft.pending.filter((p) => p.command_id && !['completed', 'failed', 'expired', 'orphaned'].includes(p.state)).map((p) => ({ session, ...p })))
      busy = true
      void (async () => {
        for (let i = 0; i < pending.length; i += 4) await Promise.all(pending.slice(i, i + 4).map(async (p) => {
          try {
            const status = await json<Operation>(`/api/v1/commands/${encodeURIComponent(p.command_id!)}`)
            if (active) update(p.session, (old) => ({ ...old, pending: old.pending.map((row) => row.id === p.id ? { ...row, state: status.state ?? row.state, turn_id: status.data?.turn_id ?? status.result?.turn_id ?? turns.current.get(p.command_id!) ?? row.turn_id } : row) }))
          } catch { /* Poll failure leaves the receipt intact for the next check. */ }
        }))
      })().finally(() => { busy = false })
    }, 1500)
    return () => { active = false; clearInterval(timer) }
  }, [update])
  const send = async (session: string, visibleTurn?: string) => {
    const draft = values.current[session] ?? empty
    const text = draft.text.trim(); if (!text || locks.current.has(session)) return
    const delivery = visibleTurn ? draft.delivery : 'queue'
    const retry = draft.pending.find((p) => p.state === 'unconfirmed' && p.text === text && p.delivery === delivery)
    if (retry?.delivery === 'steer' && retry.visible_turn_id !== visibleTurn) { update(session, (old) => ({ ...old, error: '活动对话已改变，不能将重试指令发送到另一轮；请先核对原操作' })); return }
    const pending: PendingMessage = retry ? { ...retry, state: 'submitting' } : { id: crypto.randomUUID(), text, state: 'submitting', delivery, visible_turn_id: visibleTurn }
    locks.current.add(session)
    update(session, (old) => ({ ...old, sending: true, error: undefined, pending: [...old.pending.filter((p) => p.id !== pending.id), pending] }))
    try {
      const receipt = await sendMessage(csrf, session, text, delivery, visibleTurn)
      update(session, (old) => ({ ...old, text: old.text.trim() === text ? '' : old.text, pending: old.pending.map((p) => p.id === pending.id ? { ...p, command_id: receipt.command_id, state: receipt.state ?? 'accepted', turn_id: receipt.command_id ? turns.current.get(receipt.command_id) : undefined } : p) }))
    } catch (reason) {
      const state = reason instanceof ApiError && reason.status >= 400 && reason.status < 500 ? 'rejected' : 'unconfirmed'
      update(session, (old) => ({ ...old, error: errorText(reason), pending: old.pending.map((p) => p.id === pending.id ? { ...p, state } : p) }))
    } finally { locks.current.delete(session); update(session, (old) => ({ ...old, sending: false })) }
  }
  return { get: (id: string) => drafts[id] ?? empty, update, send }
}
