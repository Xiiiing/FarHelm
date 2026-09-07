import { ArrowDownOutlined, ArrowLeftOutlined, CalendarOutlined, ClockCircleOutlined, CopyOutlined, MenuOutlined, ReloadOutlined, SendOutlined, StopOutlined } from '@ant-design/icons'
import { Alert, Button, Empty, Input, Modal, Radio, Skeleton, Space, Tooltip } from 'antd'
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react'
import { ApiError, fetchSessionDisplay, fetchTranscript, interruptSession, json, waitForCommand, type CodexSession, type TranscriptPage, type TranscriptTurn } from '../../api/features'
import { subscribeEvents } from '../../api/events'
import { mergeDelta, mergeTurns } from '../../api/transcript'
import { Transcript } from './Transcript'
import { errorText, sessionName, stateNames } from './presentation'
import type { SessionDraft } from './useOperations'

type Props = { csrf: string; id?: string; session?: CodexSession; draft: SessionDraft; onDraft: (change: (old: SessionDraft) => SessionDraft) => void; onSend: (turn?: string) => void; onRail: () => void; onCollapse: () => void; collapsed: boolean; onSchedule: (session: CodexSession) => void; onSchedules: (session: CodexSession) => void }
export function Conversation({ csrf, id, session: listed, draft, onDraft, onSend, onRail, onCollapse, collapsed, onSchedule, onSchedules }: Props) {
  const [metadata, setMetadata] = useState<CodexSession>()
  const session = metadata ? { ...listed, ...metadata, display_label: listed?.display_label ?? metadata.display_label } : listed
  const listedRef = useRef(listed); listedRef.current = listed
  const [turns, setTurns] = useState<TranscriptTurn[]>([])
  const [page, setPage] = useState<TranscriptPage>()
  const [loading, setLoading] = useState(false)
  const [failure, setFailure] = useState<Error>()
  const [ready, setReady] = useState(false)
  const readyRef = useRef(ready); readyRef.current = ready
  const [newMessages, setNewMessages] = useState(false)
  const [away, setAway] = useState(false)
  const scroll = useRef<HTMLDivElement>(null)
  const follow = useRef(true)
  const anchor = useRef<{ key: string; offset: number } | undefined>(undefined)
  const active = useRef(true)
  const generation = useRef(0)
  const metadataGeneration = useRef(0)
  const pageRef = useRef<TranscriptPage>(undefined)
  const loadingRef = useRef(false)
  const offlineRetries = useRef(0)
  const refreshQueue = useRef<{ timer?: ReturnType<typeof setTimeout>; history: boolean; metadata: boolean }>({ history: false, metadata: false })
  const capture = useCallback(() => {
    const node = scroll.current; if (!node || follow.current) { anchor.current = undefined; return }
    const top = node.getBoundingClientRect().top
    const items = node.querySelectorAll<HTMLElement>('[data-message-key]')
    let low = 0; let high = items.length
    while (low < high) { const mid = (low + high) >>> 1; if (items[mid].getBoundingClientRect().bottom > top) high = mid; else low = mid + 1 }
    const first = items[low]
    anchor.current = first ? { key: first.dataset.messageKey!, offset: first.getBoundingClientRect().top - top } : undefined
  }, [])
  const restore = useCallback(() => {
    const node = scroll.current; if (!node) return
    if (follow.current) { node.scrollTop = node.scrollHeight; return }
    const saved = anchor.current
    if (saved) {
      const target = node.querySelector<HTMLElement>(`[data-message-key="${CSS.escape(saved.key)}"]`)
      if (target) node.scrollTop += target.getBoundingClientRect().top - node.getBoundingClientRect().top - saved.offset
    }
  }, [])
  const load = useCallback(async (older = false) => {
    if (!id || (older && loadingRef.current)) return
    capture(); const epoch = ++generation.current; loadingRef.current = true; setLoading(true)
    try {
      const next = await fetchTranscript(id, older ? pageRef.current?.next_cursor : undefined)
      if (!active.current || epoch !== generation.current) return
      setTurns((old) => mergeTurns(old, [...next.turns].reverse(), older))
      setPage(next); pageRef.current = next; setFailure(undefined); setReady(true); offlineRetries.current = 0
    } catch (reason) { if (active.current && epoch === generation.current) setFailure(reason instanceof Error ? reason : new Error(errorText(reason))) }
    finally { if (active.current && epoch === generation.current) { setLoading(false); loadingRef.current = false } }
  }, [capture, id])
  const readMetadata = useCallback(async () => {
    if (!id) return
    const epoch = ++metadataGeneration.current
    try {
      const value = await json<CodexSession>(`/api/v1/codex/sessions/${encodeURIComponent(id)}`)
      if (active.current && epoch === metadataGeneration.current) setMetadata(value)
      if (!listedRef.current) {
        const display = await fetchSessionDisplay(csrf, { mode: 'labels', session_ids: [id] })
        if (active.current && epoch === metadataGeneration.current) setMetadata({ ...value, display_label: display.sessions.find((s) => s.session_id === id)?.display_label })
      }
    } catch { /* History explains unavailable targets; labels are optional. */ }
  }, [csrf, id])
  const scheduleRefresh = useCallback((history = true, metadata = true) => {
    const queue = refreshQueue.current
    queue.history ||= history; queue.metadata ||= metadata
    queue.timer ??= setTimeout(() => {
      queue.timer = undefined
      const history = queue.history; const metadata = queue.metadata
      queue.history = false; queue.metadata = false
      if (history) void load()
      if (metadata) void readMetadata()
    }, 200)
  }, [load, readMetadata])
  const completedOperations = draft.pending.filter((p) => ['completed', 'failed', 'orphaned'].includes(p.state)).map((p) => p.id).join(',')
  useEffect(() => { if (completedOperations) scheduleRefresh(true, false) }, [completedOperations, scheduleRefresh])
  useEffect(() => {
    active.current = true
    const timer = setTimeout(() => {
      if (id) {
        void load()
        void readMetadata()
      }
    }, 0)
    const queue = refreshQueue.current
    return () => { active.current = false; clearTimeout(timer); clearTimeout(queue.timer); queue.timer = undefined }
  }, [id, load, readMetadata])
  useEffect(() => {
    if (!(failure instanceof ApiError) || failure.code !== 'agent_offline') return
    const retry = () => { if (!document.hidden && !loadingRef.current) void load() }
    const timer = setTimeout(retry, Math.min(30000, 2000 * 2 ** Math.min(offlineRetries.current++, 4)))
    const visible = () => { if (!document.hidden) { clearTimeout(timer); retry() } }
    document.addEventListener('visibilitychange', visible)
    return () => { clearTimeout(timer); document.removeEventListener('visibilitychange', visible) }
  }, [failure, load])
  useEffect(() => {
    let flush: ReturnType<typeof setTimeout> | undefined
    let deltas: Parameters<typeof mergeDelta>[1][] = []
    const commit = () => {
      if (flush) clearTimeout(flush); flush = undefined
      const pending = deltas; deltas = []
      if (pending.length) {
        capture(); setTurns((old) => pending.reduce(mergeDelta, old))
        if (!follow.current) setNewMessages(true)
      }
    }
    const off = subscribeEvents(['open', 'codex.stream.resync', 'codex.session.updated', 'codex.turn.started', 'codex.turn.completed', 'codex.turn.failed', 'codex.turn.orphaned', 'codex.message.delta'], (event) => {
      if (!id) return
      if (event.type === 'open' || event.type === 'codex.stream.resync') { scheduleRefresh(); return }
      try {
        const { payload } = JSON.parse(event.data) as { payload?: { session_id?: string; data?: { turn_id?: string; item_id?: string; delta?: string; text_offset?: number } } }
        if (payload?.session_id !== id) return
        const data = payload.data
        if (event.type === 'codex.message.delta' && data?.turn_id && data.item_id && data.delta && Number.isSafeInteger(data.text_offset) && data.text_offset! >= 0) {
          deltas.push({ turn_id: data.turn_id, item_id: data.item_id, delta: data.delta, text_offset: data.text_offset! })
          flush ??= setTimeout(commit, 80)
        } else if (event.type.startsWith('codex.turn.') || event.type === 'codex.session.updated') {
          commit()
          const needsHistory = event.type.startsWith('codex.turn.') || !readyRef.current
          if (needsHistory && !follow.current) setNewMessages(true)
          scheduleRefresh(needsHistory)
        }
      } catch { /* Invalid frames never become anonymous messages. */ }
    })
    return () => { off(); if (flush) clearTimeout(flush) }
  }, [capture, id, scheduleRefresh])
  useLayoutEffect(restore, [turns, draft.pending, restore])
  useEffect(() => {
    const node = scroll.current; if (!node) return
    const observer = new ResizeObserver(restore)
    for (const child of node.children) observer.observe(child)
    return () => observer.disconnect()
  }, [restore, turns, ready])

  const interrupt = () => {
    if (!session?.active_turn_id) return
    const target = session
    Modal.confirm({ title: '中断当前对话？', content: <><p>{sessionName(target)}</p><p>轮次 {target.active_turn_id}。本次回复将停止，已执行的修改不会撤销。</p></>, okText: '确认中断', cancelText: '取消', okButtonProps: { danger: true }, onOk: async () => {
      try { await waitForCommand(await interruptSession(csrf, target.session_id, target.active_turn_id)); if (active.current) void load() }
      catch (error) { onDraft((old) => ({ ...old, error: errorText(error) })); throw error }
    } })
  }
  const failureCode = failure instanceof ApiError ? failure.code : undefined
  const unavailable = failureCode === 'agent_offline' ? 'Agent 离线，暂时无法读取历史' : failureCode === 'session_not_found' ? '会话不存在或尚未导入' : '对话暂时无法读取'
  const visibleTurn = session?.active_turn_id
  const pending = draft.pending.filter((p) => p.delivery === 'steer' ? !p.command_id : !p.turn_id || !turns.some((t) => t.turn_id === p.turn_id && t.items.some((i) => i.kind === 'user_message')))
  const continuation = page?.continuation
  const needsMessage = continuation?.kind === 'message' && !turns.some((t) => t.turn_id === continuation.turn_id && t.items.some((i) => i.item_id === continuation.item_id && i.text_complete))
  return <section className="codex-conversation">
    <header className="conversation-head"><div className="conversation-identity"><Button className="mobile-only" type="text" icon={<MenuOutlined />} onClick={onRail} aria-label="打开会话列表" /><Button className="desktop-only" type="text" icon={collapsed ? <MenuOutlined /> : <ArrowLeftOutlined />} onClick={onCollapse} aria-label={collapsed ? '展开会话列表' : '折叠会话列表'} /><div className="conversation-title"><strong>{session ? sessionName(session) : id ? '正在读取会话…' : '选择一个会话'}</strong><div className="conversation-meta">{session ? `${session.agent_id} / ${session.project_id} · ${session.mode === 'edit' ? '编辑' : '只读'} · ${stateNames[session.state] ?? '状态未知'}` : '查看历史，继续你的工作'}</div></div></div><Space size={4}>{id && <Tooltip title={`复制会话 ID：${id}`}><Button type="text" icon={<CopyOutlined />} aria-label="复制会话 ID" onClick={() => void navigator.clipboard.writeText(id).catch(() => onDraft((old) => ({ ...old, error: `复制失败，会话 ID：${id}` })))} /></Tooltip>}<Button icon={<CalendarOutlined />} disabled={!session} onClick={() => session && onSchedules(session)} aria-label="定时任务"><span className="desktop-only">定时任务</span></Button><Button type="text" icon={<ReloadOutlined />} disabled={!id} loading={loading} onClick={() => void load()} aria-label="刷新对话" /></Space></header>
    <div className="conversation-alerts">{draft.error && <Alert showIcon closable type="warning" title="指令尚未完成提交" description={draft.error} onClose={() => onDraft((old) => ({ ...old, error: undefined }))} />}{failure && turns.length > 0 && <Alert showIcon type="warning" title="历史刷新失败，已保留当前内容" description={failure.message} action={<Button onClick={() => void load()}>重试</Button>} />}</div>
    <div className="conversation-history"><div className="conversation-scroll" ref={scroll} onScroll={() => { const node = scroll.current; if (!node) return; follow.current = node.scrollHeight - node.scrollTop - node.clientHeight < 80; setAway(!follow.current); if (follow.current) setNewMessages(false); capture() }}>
      {!id ? <Empty className="codex-empty" description="从会话列表选择一个会话，或新建会话开始工作" /> : loading && !ready && !turns.length ? <div className="history-loading" role="status"><span>正在读取对话历史…</span><Skeleton active paragraph={{ rows: 5 }} /></div> : failure && !turns.length ? <Empty className="codex-empty" description={<><p>{unavailable}</p><p className="conversation-meta">{failure.message}</p></>}><Button onClick={() => void load()} icon={<ReloadOutlined />}>重试读取</Button></Empty> : ready && !turns.length ? <Empty className="codex-empty" description="这个会话还没有对话，发送第一条指令开始" /> : null}
      {page?.next_cursor && !needsMessage && <Button loading={loading} className="load-earlier" onClick={() => void load(true)}>加载更早对话</Button>}
      <Transcript turns={turns} continuation={needsMessage ? continuation : undefined} loading={loading} onContinue={() => void load(true)} onAnchor={capture} />
      {pending.map((p) => <article key={p.id} className="codex-message user pending-message" data-message-key={`pending:${p.id}`}><div className="message-role">你 · {stateNames[p.state] ?? '处理中'}</div><div className="message-body user-text">{p.text}</div></article>)}
    </div><Button className="jump-to-bottom" icon={<ArrowDownOutlined />} hidden={!away && !newMessages} onClick={() => { follow.current = true; anchor.current = undefined; restore(); setNewMessages(false); setAway(false) }}>{newMessages ? '有新消息 · 回到底部' : '回到底部'}</Button></div>
    <footer className="composer-wrap"><div className="composer"><Input.TextArea aria-label="给 Codex 发送指令" value={draft.text} onChange={(event) => onDraft((old) => ({ ...old, text: event.target.value }))} autoSize={{ minRows: 2, maxRows: 5 }} maxLength={32768} placeholder={session ? '给 Codex 发送指令…' : '请先选择会话'} disabled={!session || failureCode === 'session_not_found'} onPressEnter={(event) => { if (!event.shiftKey && !event.nativeEvent.isComposing && event.keyCode !== 229) { event.preventDefault(); onSend(visibleTurn) } }} />
      {visibleTurn && <Radio.Group className="delivery-options" aria-label="活动会话发送方式" value={draft.delivery} onChange={(event) => onDraft((old) => ({ ...old, delivery: event.target.value as 'queue' | 'steer' }))}><Radio value="queue">排队下一轮</Radio><Radio value="steer">补充当前对话</Radio></Radio.Group>}
      <div className="composer-actions"><Space size={4}><Button type="text" icon={<ClockCircleOutlined />} disabled={!session} onClick={() => session && onSchedule(session)}>定时发送</Button>{visibleTurn && <Button danger type="text" icon={<StopOutlined />} onClick={interrupt} aria-label="中断">中断</Button>}</Space><Button type="primary" icon={<SendOutlined />} loading={draft.sending} disabled={!session || !draft.text.trim()} onClick={() => onSend(visibleTurn)} aria-label="发送指令">发送</Button></div>
    </div><div className="composer-hint" role="status">{draft.sending ? '正在提交，等待 Agent 保存确认…' : draft.pending.at(-1)?.command_id ? `${stateNames[draft.pending.at(-1)!.state] ?? '处理中'} · 指令 ${draft.pending.at(-1)!.command_id!.slice(-8)}` : 'Enter 发送 · Shift + Enter 换行'}</div></footer>
  </section>
}
