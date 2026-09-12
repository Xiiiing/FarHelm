import { useCallback, useEffect, useRef, useState } from 'react'
import { useQueries } from '@tanstack/react-query'
import { ApiError, json, sendMessage, operateNative, waitForCommand, type ModelChoice, type Operation } from '../../api/features'
import { cacheOperation, keys, queryClient } from '../../api/cache'
import { errorText } from './presentation'
import { setUnsavedDraftCount } from '../../pwaUpdate'
import type { DraftImage } from './NativeComposer'
export type PendingMessage = { native_queue?: boolean; id: string; text: string; state: string; detail?: string; command_id?: string; turn_id?: string; visible_turn_id?: string; delivery: 'queue' | 'steer'; model_choice?: ModelChoice }
export type SessionDraft = { skills?: string[]; images?: DraftImage[]; text: string; error?: string; sending: boolean; delivery: 'queue' | 'steer'; pending: PendingMessage[]; model_choice?: ModelChoice }
const empty: SessionDraft = { text: '', sending: false, delivery: 'queue', pending: [] }

export function useOperations(csrf: string) {
  const [drafts, setDrafts] = useState<Record<string, SessionDraft>>({})
  const values = useRef(drafts); const locks = useRef(new Set<string>())
  useEffect(() => { setUnsavedDraftCount(Object.values(drafts).filter((draft) => draft.text.trim() || draft.images?.length || draft.skills?.length).length) }, [drafts])
  useEffect(() => () => { setUnsavedDraftCount(0); for (const draft of Object.values(values.current)) for (const image of draft.images ?? []) URL.revokeObjectURL(image.url) }, [])
  const update = useCallback((session: string, change: (old: SessionDraft) => SessionDraft) => {
    const next = { ...values.current, [session]: change(values.current[session] ?? empty) }
    values.current = next; setDrafts(next)
  }, [])
  const ids = Object.values(drafts).flatMap((draft) => draft.pending.flatMap((row) => row.command_id ? [row.command_id] : []))
  useQueries({ queries: ids.map((id) => ({ queryKey: keys.operation(id), staleTime: Infinity, queryFn: ({ signal }: { signal: AbortSignal }) => json<Operation>(`/api/v1/commands/${encodeURIComponent(id)}`, signal) })) })
  const send = async (session: string, visibleTurn?: string, nativeQueue = false) => {
    const draft = values.current[session] ?? empty
    const text = draft.text.trim(); if (!text || locks.current.has(session)) return
    const delivery = visibleTurn ? draft.delivery : 'queue'
    const nativeInput = !!draft.skills?.length || !!draft.images?.length
    if (nativeInput && (delivery === 'steer' || draft.images?.some(image => image.state !== 'ready'))) { update(session, old => ({ ...old, error: 'Skills 和图片需要完成上传后排队发送。' })); return }
    const retry = draft.pending.find((p) => p.state === 'unconfirmed' && p.text === text && p.delivery === delivery)
    if (retry?.delivery === 'steer' && retry.visible_turn_id !== visibleTurn) { update(session, (old) => ({ ...old, error: '活动对话已改变，不能将重试指令发送到另一轮；请先核对原操作' })); return }
    const pending: PendingMessage = retry ? { ...retry, state: 'submitting' } : { id: crypto.randomUUID(), text, state: 'submitting', delivery, visible_turn_id: visibleTurn, model_choice: delivery === 'queue' ? draft.model_choice : undefined }
    const useNativeQueue = retry?.native_queue ?? (nativeInput || (nativeQueue && delivery === 'queue'))
    pending.native_queue = useNativeQueue
    locks.current.add(session)
    update(session, (old) => ({ ...old, sending: true, error: undefined, pending: [...old.pending.filter((p) => p.id !== pending.id), pending] }))
    try {
      if (useNativeQueue && pending.model_choice) await waitForCommand(await operateNative(csrf, session, { operation: 'thread_settings', settings: pending.model_choice }))
      const receipt = useNativeQueue
        ? await operateNative(csrf, session, { operation: 'queue_add', client_message_id: pending.id, input: [...(draft.skills ?? []).map(skill_id => ({ type: 'skill' as const, skill_id })), ...(draft.images ?? []).map(image => ({ type: 'image' as const, attachment_id: image.id })), { type: 'text', text }] })
        : await sendMessage(csrf, session, text, delivery, visibleTurn, pending.id, pending.model_choice)
      if (receipt.command_id) cacheOperation(receipt.command_id, receipt)
      if (nativeInput) for (const image of draft.images ?? []) URL.revokeObjectURL(image.url)
      update(session, (old) => ({ ...old, text: old.text.trim() === text ? '' : old.text, skills: nativeInput ? [] : old.skills, images: nativeInput ? [] : old.images, pending: old.pending.map((p) => p.id === pending.id ? { ...p, command_id: receipt.command_id, state: receipt.state ?? 'accepted' } : p) }))
    } catch (reason) {
      const state = reason instanceof ApiError && reason.status >= 400 && reason.status < 500 ? 'rejected' : 'unconfirmed'
      update(session, (old) => ({ ...old, error: errorText(reason), pending: old.pending.map((p) => p.id === pending.id ? { ...p, state } : p) }))
    } finally { locks.current.delete(session); update(session, (old) => ({ ...old, sending: false })) }
  }
  return { get: (id: string): SessionDraft => {
    const draft = drafts[id] ?? empty
    return { ...draft, pending: draft.pending.map((row) => {
      const receipt = row.command_id ? queryClient.getQueryData<Operation>(keys.operation(row.command_id)) : undefined
      const turn = row.native_queue ? queryClient.getQueryData<import('../../api/features').TranscriptPage>(keys.history(id))?.turns.find(turn => turn.items.some(item => item.kind === 'user_message' && item.client_id === row.id)) : undefined
      return receipt ? { ...row, state: turn?.status ?? (row.native_queue && receipt.state === 'completed' ? 'queued' : receipt.state ?? row.state), detail: receipt.detail, turn_id: turn?.turn_id ?? receipt.data?.turn_id ?? receipt.result?.turn_id ?? row.turn_id } : row
    }) }
  }, update, send }
}
