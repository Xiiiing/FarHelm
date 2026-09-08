import { useCallback, useRef, useState } from 'react'
import { useQueries } from '@tanstack/react-query'
import { ApiError, json, sendMessage, type ModelChoice, type Operation } from '../../api/features'
import { cacheOperation, keys, queryClient } from '../../api/cache'
import { errorText } from './presentation'
export type PendingMessage = { id: string; text: string; state: string; detail?: string; command_id?: string; turn_id?: string; visible_turn_id?: string; delivery: 'queue' | 'steer'; model_choice?: ModelChoice }
export type SessionDraft = { text: string; error?: string; sending: boolean; delivery: 'queue' | 'steer'; pending: PendingMessage[]; model_choice?: ModelChoice }
const empty: SessionDraft = { text: '', sending: false, delivery: 'queue', pending: [] }

export function useOperations(csrf: string) {
  const [drafts, setDrafts] = useState<Record<string, SessionDraft>>({})
  const values = useRef(drafts); const locks = useRef(new Set<string>())
  const update = useCallback((session: string, change: (old: SessionDraft) => SessionDraft) => {
    const next = { ...values.current, [session]: change(values.current[session] ?? empty) }
    values.current = next; setDrafts(next)
  }, [])
  const ids = Object.values(drafts).flatMap((draft) => draft.pending.flatMap((row) => row.command_id ? [row.command_id] : []))
  useQueries({ queries: ids.map((id) => ({ queryKey: keys.operation(id), staleTime: Infinity, queryFn: ({ signal }: { signal: AbortSignal }) => json<Operation>(`/api/v1/commands/${encodeURIComponent(id)}`, signal) })) })
  const send = async (session: string, visibleTurn?: string) => {
    const draft = values.current[session] ?? empty
    const text = draft.text.trim(); if (!text || locks.current.has(session)) return
    const delivery = visibleTurn ? draft.delivery : 'queue'
    const retry = draft.pending.find((p) => p.state === 'unconfirmed' && p.text === text && p.delivery === delivery)
    if (retry?.delivery === 'steer' && retry.visible_turn_id !== visibleTurn) { update(session, (old) => ({ ...old, error: '活动对话已改变，不能将重试指令发送到另一轮；请先核对原操作' })); return }
    const pending: PendingMessage = retry ? { ...retry, state: 'submitting' } : { id: crypto.randomUUID(), text, state: 'submitting', delivery, visible_turn_id: visibleTurn, model_choice: delivery === 'queue' ? draft.model_choice : undefined }
    locks.current.add(session)
    update(session, (old) => ({ ...old, sending: true, error: undefined, pending: [...old.pending.filter((p) => p.id !== pending.id), pending] }))
    try {
      const receipt = await sendMessage(csrf, session, text, delivery, visibleTurn, pending.id, pending.model_choice)
      if (receipt.command_id) cacheOperation(receipt.command_id, receipt)
      update(session, (old) => ({ ...old, text: old.text.trim() === text ? '' : old.text, pending: old.pending.map((p) => p.id === pending.id ? { ...p, command_id: receipt.command_id, state: receipt.state ?? 'accepted' } : p) }))
    } catch (reason) {
      const state = reason instanceof ApiError && reason.status >= 400 && reason.status < 500 ? 'rejected' : 'unconfirmed'
      update(session, (old) => ({ ...old, error: errorText(reason), pending: old.pending.map((p) => p.id === pending.id ? { ...p, state } : p) }))
    } finally { locks.current.delete(session); update(session, (old) => ({ ...old, sending: false })) }
  }
  return { get: (id: string): SessionDraft => {
    const draft = drafts[id] ?? empty
    return { ...draft, pending: draft.pending.map((row) => {
      const receipt = row.command_id ? queryClient.getQueryData<Operation>(keys.operation(row.command_id)) : undefined
      return receipt ? { ...row, state: receipt.state ?? row.state, detail: receipt.detail, turn_id: receipt.data?.turn_id ?? receipt.result?.turn_id ?? row.turn_id } : row
    }) }
  }, update, send }
}
