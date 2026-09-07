import { ArrowDownOutlined, ArrowLeftOutlined, CalendarOutlined, ClockCircleOutlined, CopyOutlined, EllipsisOutlined, MenuOutlined, ReloadOutlined, SendOutlined, StopOutlined } from '@ant-design/icons'
import { Sender } from '@ant-design/x'
import { useQuery } from '@tanstack/react-query'
import { Alert, Button, Dropdown, Empty, Input, Modal, Radio, Skeleton, Space } from 'antd'
import { forwardRef, useCallback, useEffect, useLayoutEffect, useRef, useState, type ComponentProps, type ComponentRef } from 'react'
import { ApiError, fetchSessionDisplay, fetchTranscript, interruptSession, json, waitForCommand, type CodexSession, type TranscriptPage } from '../../api/features'
import { cacheHistory, keys, queryClient, mergeSession } from '../../api/cache'
import { Transcript, type TranscriptPosition } from './Transcript'
import { errorText, sessionName, stateNames } from './presentation'
import type { SessionDraft } from './useOperations'
import { useAgents } from '../../hooks/useAgents'

const ComposerInput = forwardRef<ComponentRef<typeof Input.TextArea>, ComponentProps<typeof Input.TextArea>>((props, ref) => <Input.TextArea {...props} ref={ref} aria-label="给 Codex 发送指令" maxLength={32768} />)
const senderComponents = { input: ComposerInput }
type Props = { csrf: string; id?: string; session?: CodexSession; draft: SessionDraft; onDraft: (change: (old: SessionDraft) => SessionDraft) => void; onSend: (turn?: string) => void; onRail: () => void; onCollapse: () => void; collapsed: boolean; onSchedule: (session: CodexSession) => void; onSchedules: (session: CodexSession) => void }

export function Conversation({ csrf, id, session: listed, draft, onDraft, onSend, onRail, onCollapse, collapsed, onSchedule, onSchedules }: Props) {
  const metadata = useQuery({ queryKey: keys.session(id ?? ''), enabled: !!id, queryFn: async ({ signal }) => mergeSession(await json<CodexSession>(`/api/v1/codex/sessions/${encodeURIComponent(id!)}`, signal)) })
  const display = useQuery({ queryKey: ['codex', 'label', id], enabled: !!id && !listed && !metadata.data?.title, queryFn: ({ signal }) => fetchSessionDisplay(csrf, { mode: 'labels', session_ids: [id!] }, signal) })
  const session = metadata.data ? { ...listed, ...metadata.data, display_label: listed?.display_label ?? display.data?.sessions[0]?.display_label ?? metadata.data.display_label } : listed
  const { agents } = useAgents()
  const agent = agents.state === 'ready' ? agents.data.agents.find((agent) => agent.agent_id === session?.agent_id) : undefined
  const codex = agent?.codex
  const codexNotice = agent?.online && codex && codex.state !== 'ready' ? codex.state === 'starting' ? 'Codex 正在初始化，实验上报正常运行' : codex.state === 'login_required' ? '请在服务器上登录 Codex，登录后会自动检查连接' : codex.reason === 'codex_not_configured' || codex.reason === 'codex_not_found' || codex.reason === 'codex_configured_binary_unavailable' ? '未找到已配置的 Codex，请在服务器运行 farhelm-agent codex configure 后重启 Agent' : 'Codex 暂时不可用，正在重新检查连接；可在服务器运行 farhelm-agent doctor 查看原因' : undefined
  const history = useQuery({ queryKey: keys.history(id ?? ''), enabled: !!id, staleTime: 0, refetchOnMount: 'always', queryFn: async ({ signal }) => cacheHistory(id!, await fetchTranscript(id!, undefined, signal)) })
  const page = history.data; const turns = page?.turns ?? []
  const [readingMore, setReadingMore] = useState(false)
  const [moreError, setMoreError] = useState<Error>()
  const [newMessages, setNewMessages] = useState(false)
  const [away, setAway] = useState(false)
  const scroll = useRef<HTMLDivElement>(null)
  const follow = useRef(true)
  const active = useRef(true)
  const continuationRead = useRef<AbortController | undefined>(undefined)
  const anchor = useRef<{ key: string; turn: string; offset: number } | undefined>(undefined)
  const position = useRef<TranscriptPosition | undefined>(undefined)
  const restoring = useRef(false)
  const anchorLocked = useRef(false)
  const capture = useCallback(() => {
    const node = scroll.current; if (!node || follow.current) { anchor.current = undefined; return }
    const top = node.getBoundingClientRect().top
    const first = [...node.querySelectorAll<HTMLElement>('[data-message-key]')].find((item) => item.getBoundingClientRect().bottom > top)
    anchor.current = first ? { key: first.dataset.messageKey!, turn: first.closest<HTMLElement>('[data-turn-id]')?.dataset.turnId ?? '', offset: first.getBoundingClientRect().top - top } : undefined
  }, [])
  const restore = useCallback(() => {
    const node = scroll.current; if (!node) return
    if (follow.current) { node.scrollTop = node.scrollHeight; return }
    const saved = anchor.current
    if (saved) {
      const target = node.querySelector<HTMLElement>(`[data-message-key="${CSS.escape(saved.key)}"]`)
      if (target) { node.scrollTop += target.getBoundingClientRect().top - node.getBoundingClientRect().top - saved.offset; restoring.current = false; requestAnimationFrame(() => requestAnimationFrame(() => { anchorLocked.current = false })) }
      else if (!restoring.current && position.current?.reveal(saved.turn)) {
        restoring.current = true
        requestAnimationFrame(() => {
          const target = node.querySelector<HTMLElement>(`[data-message-key="${CSS.escape(saved.key)}"]`)
          if (target) node.scrollTop += target.getBoundingClientRect().top - node.getBoundingClientRect().top - saved.offset
          restoring.current = false
          requestAnimationFrame(() => requestAnimationFrame(() => { anchorLocked.current = false }))
        })
      }
    }
  }, [])
  useEffect(() => { active.current = true; return () => { active.current = false; continuationRead.current?.abort() } }, [])
  useLayoutEffect(() => { restore() }, [page, draft.pending, restore])
  const previousPage = useRef(page)
  const hasPage = !!page
  useEffect(() => {
    if (page && previousPage.current && page !== previousPage.current && !follow.current) setNewMessages(true)
    previousPage.current = page
  }, [page])
  useEffect(() => {
    const node = scroll.current; if (!node) return
    const observer = new ResizeObserver(restore)
    for (const child of node.children) observer.observe(child)
    return () => observer.disconnect()
  }, [restore, hasPage])
  const load = async (older = false) => {
    if (!id) return
    capture()
    if (!older) { setMoreError(undefined); await history.refetch(); return }
    if (continuationRead.current || !page?.next_cursor) return
    anchorLocked.current = !!anchor.current
    const controller = new AbortController(); continuationRead.current = controller; setReadingMore(true)
    try {
      const next = await fetchTranscript(id, page.next_cursor, controller.signal)
      if (!controller.signal.aborted) { queryClient.setQueryData<TranscriptPage>(keys.history(id), cacheHistory(id, next, true)); setMoreError(undefined) }
    } catch (reason) { anchorLocked.current = false; if (!controller.signal.aborted) setMoreError(reason instanceof Error ? reason : new Error(errorText(reason))) }
    finally { continuationRead.current = undefined; if (active.current) setReadingMore(false) }
  }
  const interrupt = () => {
    if (!session?.active_turn_id) return
    const target = session
    Modal.confirm({ title: '中断当前对话？', content: <><p>{sessionName(target)}</p><p>轮次 {target.active_turn_id}。本次回复将停止，已执行的修改不会撤销。</p></>, okText: '确认中断', cancelText: '取消', okButtonProps: { danger: true }, onOk: async () => {
      try { await waitForCommand(await interruptSession(csrf, target.session_id, target.active_turn_id)) }
      catch (error) { if (active.current) onDraft((old) => ({ ...old, error: errorText(error) })); throw error }
    } })
  }
  const failure = moreError ?? history.error
  const failureCode = failure instanceof ApiError ? failure.code : undefined
  const unavailable = failureCode === 'agent_offline' ? 'Agent 离线，暂时无法读取历史' : failureCode === 'session_not_found' ? '会话不存在或尚未导入' : '对话暂时无法读取'
  const loading = history.isFetching || readingMore
  const visibleTurn = session?.active_turn_id
  const pending = draft.pending.filter((p) => p.delivery === 'steer' ? !p.command_id : !p.turn_id || !turns.some((t) => t.turn_id === p.turn_id && t.items.some((i) => i.kind === 'user_message')))
  const continuation = page?.continuation
  const needsMessage = continuation?.kind === 'message' && !turns.some((t) => t.turn_id === continuation.turn_id && t.items.some((i) => i.item_id === continuation.item_id && i.text_complete))
  const menu = { items: [{ key: 'schedules', icon: <CalendarOutlined />, label: '定时任务', disabled: !session }, { key: 'copy', icon: <CopyOutlined />, label: '复制会话 ID', disabled: !id }], onClick: ({ key }: { key: string }) => {
    if (key === 'schedules' && session) onSchedules(session)
    if (key === 'copy' && id) void navigator.clipboard.writeText(id).catch(() => onDraft((old) => ({ ...old, error: `复制失败，会话 ID：${id}` })))
  } }
  return <section className="codex-conversation">
    <header className="conversation-head"><div className="conversation-identity"><Button className="mobile-only" type="text" icon={<MenuOutlined />} onClick={onRail} aria-label="打开会话列表" /><Button className="desktop-only" type="text" icon={collapsed ? <MenuOutlined /> : <ArrowLeftOutlined />} onClick={onCollapse} aria-label={collapsed ? '展开会话列表' : '折叠会话列表'} /><div className="conversation-title"><strong>{session ? sessionName(session) : id ? '正在读取会话…' : '选择一个会话'}</strong><div className="conversation-meta">{session ? `${session.agent_id} / ${session.project_id} · ${session.mode === 'edit' ? '编辑' : '只读'}` : '查看历史，继续你的工作'}</div></div></div><Space size={4}><Button type="text" icon={<ReloadOutlined />} disabled={!id} loading={history.isFetching} onClick={() => void load()} aria-label="刷新对话" /><Dropdown menu={menu} trigger={['click']}><Button type="text" icon={<EllipsisOutlined />} aria-label="会话操作" /></Dropdown></Space></header>
    <div className="conversation-alerts">{codexNotice && <Alert showIcon type={codex?.state === 'starting' ? 'info' : 'warning'} title={codexNotice} />}{draft.error && <Alert showIcon closable type="warning" title="指令尚未完成提交" description={draft.error} onClose={() => onDraft((old) => ({ ...old, error: undefined }))} />}{failure && turns.length > 0 && <Alert showIcon type="warning" title="历史刷新失败，已保留当前内容" description={failure.message} action={<Button onClick={() => void load(!!moreError)}>重试</Button>} />}</div>
    <div className="conversation-history"><div className="conversation-scroll" ref={scroll} onWheel={() => { anchorLocked.current = false }} onTouchMove={() => { anchorLocked.current = false }} onScroll={() => { const node = scroll.current; if (!node || restoring.current || anchorLocked.current) return; follow.current = node.scrollHeight - node.scrollTop - node.clientHeight < 80; setAway(!follow.current); if (follow.current) setNewMessages(false); capture() }}>
      {!id ? <Empty className="codex-empty" description="从会话列表选择一个会话，或新建会话开始工作" /> : loading && !page ? <div className="history-loading" role="status"><span>正在读取对话历史…</span><Skeleton active paragraph={{ rows: 5 }} /></div> : failure && !turns.length ? <Empty className="codex-empty" description={<><p>{unavailable}</p><p className="conversation-meta">{failure.message}</p></>}><Button onClick={() => void load()} icon={<ReloadOutlined />}>重试读取</Button></Empty> : page && !turns.length ? <Empty className="codex-empty" description="这个会话还没有对话，发送第一条指令开始" /> : null}
      {page?.next_cursor && !needsMessage && <Button loading={loading} className="load-earlier" onClick={() => void load(true)}>加载更早对话</Button>}
      <Transcript turns={turns} scroll={scroll} position={position} continuation={needsMessage ? continuation : undefined} loading={loading} onContinue={() => void load(true)} onAnchor={capture} />
      <div className="pending-messages">{pending.map((p) => <article key={p.id} className="codex-message user pending-message" data-message-key={`pending:${p.id}`}><div className="message-role">你 · {stateNames[p.state] ?? '处理中'}</div><div className="message-body user-text">{p.text}</div></article>)}</div>
    </div><Button className="jump-to-bottom" icon={<ArrowDownOutlined />} hidden={!away && !newMessages} onClick={() => { follow.current = true; anchor.current = undefined; restore(); setNewMessages(false); setAway(false) }}>{newMessages ? '有新消息 · 回到底部' : '回到底部'}</Button></div>
    <footer className="composer-wrap"><Sender className="composer" components={senderComponents} value={draft.text} onChange={(value) => onDraft((old) => ({ ...old, text: value }))} autoSize={{ minRows: 2, maxRows: 5 }} placeholder={session ? '给 Codex 发送指令…' : '请先选择会话'} disabled={!session || failureCode === 'session_not_found'} onSubmit={() => onSend(visibleTurn)} onKeyDown={(event) => { if (event.nativeEvent.isComposing || event.keyCode === 229) return false }} suffix={false} footer={(_, { components: { SendButton } }) => <>
      {visibleTurn && <Radio.Group className="delivery-options" aria-label="活动会话发送方式" value={draft.delivery} onChange={(event) => onDraft((old) => ({ ...old, delivery: event.target.value as 'queue' | 'steer' }))}><Radio value="queue">排队下一轮</Radio><Radio value="steer">补充当前对话</Radio></Radio.Group>}
      <div className="composer-actions"><Space size={4}><Button type="text" icon={<ClockCircleOutlined />} disabled={!session} onClick={() => session && onSchedule(session)}>定时发送</Button>{visibleTurn && <Button danger type="text" icon={<StopOutlined />} onClick={interrupt} aria-label="中断">中断</Button>}</Space><SendButton shape="default" type="primary" icon={<SendOutlined />} loading={draft.sending} disabled={!session || !draft.text.trim() || draft.sending} aria-label="发送指令">发送</SendButton></div>
    </>} /><div className="composer-hint" role="status">{draft.sending ? '正在提交，等待 Agent 保存确认…' : draft.pending.at(-1)?.command_id ? `${stateNames[draft.pending.at(-1)!.state] ?? '处理中'} · 指令 ${draft.pending.at(-1)!.command_id!.slice(-8)}` : 'Enter 发送 · Shift + Enter 换行'}</div></footer>
  </section>
}
