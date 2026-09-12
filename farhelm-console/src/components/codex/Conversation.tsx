import { ArrowDownOutlined, ArrowUpOutlined, CalendarOutlined, CheckCircleOutlined, ClockCircleOutlined, CloseCircleOutlined, CopyOutlined, EllipsisOutlined, FolderOpenOutlined, LoadingOutlined, MenuFoldOutlined, MenuOutlined, MenuUnfoldOutlined, ReloadOutlined, StopOutlined } from '@ant-design/icons'
import { Sender } from '@ant-design/x'
import { useQuery } from '@tanstack/react-query'
import { Alert, Button, Dropdown, Empty, Input, Modal, Radio, Skeleton, Space, Tooltip } from 'antd'
import { forwardRef, useCallback, useEffect, useLayoutEffect, useRef, useState, type ComponentProps, type ComponentRef } from 'react'
import { ApiError, codexOperationError, fetchSessionDisplay, fetchTranscript, interruptSession, json, waitForCommand, type CodexSession, type TranscriptPage } from '../../api/features'
import { cacheHistory, keys, queryClient, mergeSession } from '../../api/cache'
import { ActivityIndicator, Welcome } from './Welcome'
import { Transcript, type TranscriptPosition } from './Transcript'
import { errorText, sessionName, stateNames } from './presentation'
import type { SessionDraft } from './useOperations'
import { useAgents } from '../../hooks/useAgents'
import { ModelPicker, PermissionDetails } from './SessionSettings'
import { NativeSession, RenameSession } from './NativeSession'
import { ArchiveDialog } from './ArchiveDialog'
import { useProjects } from '../../hooks/useProjects'
import { NativeActivity } from './NativeActivity'
import { NativeComposer } from './NativeComposer'
import { NativeControlDrawer, NativeInteractionCards } from './NativeControl'

const ComposerInput = forwardRef<ComponentRef<typeof Input.TextArea>, ComponentProps<typeof Input.TextArea>>((props, ref) => <Input.TextArea {...props} variant="borderless" ref={ref} aria-label="给 Codex 发送指令" maxLength={32768} />)
const senderComponents = { input: ComposerInput }
type Props = { recent: CodexSession[]; onSelect: (id: string) => void; onNew: () => void; onBrowse: () => void; csrf: string; id?: string; session?: CodexSession; draft: SessionDraft; onDraft: (change: (old: SessionDraft) => SessionDraft) => void; onSend: (turn?: string, nativeQueue?: boolean) => void; onRail: () => void; onCollapse: () => void; collapsed: boolean; onSchedule: (session: CodexSession) => void; onSchedules: (session: CodexSession) => void }

export function Conversation({ recent, onSelect, onNew, onBrowse, csrf, id, session: listed, draft, onDraft, onSend, onRail, onCollapse, collapsed, onSchedule, onSchedules }: Props) {
  const metadata = useQuery({ queryKey: keys.session(id ?? ''), enabled: !!id, queryFn: async ({ signal }) => mergeSession(await json<CodexSession>(`/api/v1/codex/sessions/${encodeURIComponent(id!)}`, signal)) })
  const display = useQuery({ queryKey: ['codex', 'label', id], enabled: !!id && !listed && !metadata.data?.title, queryFn: ({ signal }) => fetchSessionDisplay(csrf, { mode: 'labels', session_ids: [id!] }, signal) })
  const session = metadata.data ? { ...listed, ...metadata.data, display_label: listed?.display_label ?? display.data?.sessions[0]?.display_label ?? metadata.data.display_label } : listed
  const { agents } = useAgents()
  const agent = agents.state === 'ready' ? agents.data.agents.find((agent) => agent.agent_id === session?.agent_id) : undefined
  const projects = useProjects(csrf)
  const project = projects.approved.find((p) => p.agent_id === session?.agent_id && p.suggested_project_id === session?.project_id)
  const hidden = project && projects.preference(project).hidden
  const codex = agent?.codex
  const codexNotice = agent?.online && codex && codex.state !== 'ready' ? codex.state === 'starting' ? 'Codex 正在初始化，实验上报正常运行' : codex.state === 'login_required' ? '请在服务器上登录 Codex，登录后会自动检查连接' : codex.reason === 'codex_not_configured' || codex.reason === 'codex_not_found' || codex.reason === 'codex_configured_binary_unavailable' ? '未找到已配置的 Codex，请在服务器运行 farhelm-agent codex configure 后重启 Agent' : 'Codex 暂时不可用，正在重新检查连接；可在服务器运行 farhelm-agent doctor 查看原因' : undefined
  const history = useQuery({ queryKey: keys.history(id ?? ''), enabled: !!id, staleTime: 0, refetchOnMount: 'always', queryFn: async ({ signal }) => cacheHistory(id!, await fetchTranscript(id!, undefined, signal)) })
  const page = history.data; const turns = page?.turns ?? []
  const [readingMore, setReadingMore] = useState(false)
  const [modal, modalHolder] = Modal.useModal()
  const [sessionDialog, setSessionDialog] = useState<'rename' | 'native' | 'archive' | 'control'>()
  const [moreError, setMoreError] = useState<Error>()
  const [newMessages, setNewMessages] = useState(false)
  const [away, setAway] = useState(false)
  const scroll = useRef<HTMLDivElement>(null)
  const [scrollElement, setScrollElement] = useState<HTMLDivElement | null>(null)
  // Publish the parent DOM attachment so the child Virtualizer connects before network reconciliation.
  const bindScroll = useCallback((node: HTMLDivElement | null) => { scroll.current = node; setScrollElement(node) }, [])
  const sender = useRef<ComponentRef<typeof Sender>>(null)
  const follow = useRef(true)
  const active = useRef(true)
  const continuationRead = useRef<AbortController | undefined>(undefined)
  const anchor = useRef<{ key: string; turn: string; offset: number } | undefined>(undefined)
  const position = useRef<TranscriptPosition | undefined>(undefined)
  const restoring = useRef(false)
  const anchorLocked = useRef(false)
  const restoredTop = useRef<number | undefined>(undefined)
  const capture = useCallback(() => {
    // A new reading position supersedes callbacks from the previous layout.
    restoring.current = false
    const node = scroll.current; if (!node || follow.current) { anchor.current = undefined; return }
    const top = node.getBoundingClientRect().top
    const bottom = node.getBoundingClientRect().bottom
    const first = [...node.querySelectorAll<HTMLElement>('[data-message-key]')].find((item) => { const bounds = item.getBoundingClientRect(); return bounds.bottom > top && bounds.top < bottom })
    anchor.current = first ? { key: first.dataset.messageKey!, turn: first.closest<HTMLElement>('[data-turn-id]')?.dataset.turnId ?? '', offset: first.getBoundingClientRect().top - top } : undefined
  }, [])
  const restore = useCallback(() => {
    const node = scroll.current; if (!node) return
    const move = (top: number) => { node.scrollTop = top; restoredTop.current = node.scrollTop }
    if (follow.current) { move(node.scrollHeight); return }
    const saved = anchor.current
    if (saved) {
      const target = node.querySelector<HTMLElement>(`[data-message-key="${CSS.escape(saved.key)}"]`)
      if (target) { move(node.scrollTop + target.getBoundingClientRect().top - node.getBoundingClientRect().top - saved.offset); restoring.current = false; requestAnimationFrame(() => requestAnimationFrame(() => { if (anchor.current === saved) anchorLocked.current = false })) }
      else if (!restoring.current && position.current?.reveal(saved.turn)) {
        restoredTop.current = node.scrollTop
        restoring.current = true
        requestAnimationFrame(() => {
          if (anchor.current !== saved) return
          const target = node.querySelector<HTMLElement>(`[data-message-key="${CSS.escape(saved.key)}"]`)
          if (target) move(node.scrollTop + target.getBoundingClientRect().top - node.getBoundingClientRect().top - saved.offset)
          restoring.current = false
          requestAnimationFrame(() => requestAnimationFrame(() => { if (anchor.current === saved) anchorLocked.current = false }))
        })
      }
    }
  }, [])
  useEffect(() => { active.current = true; return () => { active.current = false; continuationRead.current?.abort() } }, [])
  useLayoutEffect(() => { restore() }, [page, draft.pending, scrollElement, restore])
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
  const refetchHistory = history.refetch
  const load = useCallback(async (older = false) => {
    if (!id) return
    capture()
    if (!older) { setMoreError(undefined); await refetchHistory(); return }
    if (continuationRead.current || !page?.next_cursor) return
    anchorLocked.current = !!anchor.current
    const controller = new AbortController(); continuationRead.current = controller; setReadingMore(true)
    try {
      const next = await fetchTranscript(id, page.next_cursor, controller.signal)
      if (!controller.signal.aborted) { queryClient.setQueryData<TranscriptPage>(keys.history(id), cacheHistory(id, next, true)); setMoreError(undefined) }
    } catch (reason) { anchorLocked.current = false; if (!controller.signal.aborted) setMoreError(reason instanceof Error ? reason : new Error(errorText(reason))) }
    finally {
      // Keep the next-page action busy until the new DOM and virtual measurements
      // can restore the reading position; otherwise a fast second click races it.
      await new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve())))
      continuationRead.current = undefined; if (active.current) setReadingMore(false)
    }
  }, [id, capture, refetchHistory, page])
  const continueHistory = useCallback(() => { void load(true) }, [load])
  const anchorInteraction = useCallback(() => { follow.current = false; setAway(true); capture() }, [capture])
  const interrupt = () => {
    if (!session?.active_turn_id) return
    const target = session
    modal.confirm({ title: '中断当前对话？', content: <><p>{sessionName(target)}</p><p>轮次 {target.active_turn_id}。本次回复将停止，已执行的修改不会撤销。</p></>, okText: '确认中断', cancelText: '取消', okButtonProps: { danger: true }, onOk: async () => {
      try { await waitForCommand(await interruptSession(csrf, target.session_id, target.active_turn_id)) }
      catch (error) { if (active.current) onDraft((old) => ({ ...old, error: errorText(error) })); throw error }
    } })
  }
  const failure = moreError ?? history.error
  const failureCode = failure instanceof ApiError ? failure.code : undefined
  const unavailable = failureCode === 'agent_offline' ? 'Agent 离线，暂时无法读取历史' : failureCode === 'session_not_found' ? '会话不存在或尚未导入' : '对话暂时无法读取'
  const loading = history.isFetching || readingMore
  const visibleTurn = session?.active_turn_id
  const pending = draft.pending.filter((p) => !turns.some(turn => turn.items.some(item => item.kind === 'user_message' && item.client_id === p.id))).filter((p) => p.delivery === 'steer' ? !p.command_id : !p.turn_id || !turns.some((t) => t.turn_id === p.turn_id && t.items.some((i) => i.kind === 'user_message')))
  const continuation = page?.continuation
  const needsMessage = continuation?.kind === 'message' && !turns.some((t) => t.turn_id === continuation.turn_id && t.items.some((i) => i.item_id === continuation.item_id && i.text_complete))
  const nativeIdentity = !!session && !!agent?.capabilities?.includes('codex.native_identity')
  const nativeControl = !!session && !!agent?.capabilities?.includes('codex.native_control')
  const menu = { items: [{ key: 'control', label: '原生队列、目标与审查', disabled: !nativeControl || session?.state === 'archived' }, { key: 'archive', label: session?.state === 'archived' ? '恢复会话' : '归档会话', disabled: !session || !agent?.online || !agent.capabilities?.includes('codex.session_archive') }, { key: 'rename', label: '重命名会话', disabled: !nativeIdentity || session?.state === 'archived' }, { key: 'native', label: '在原生 Codex 中继续', disabled: !nativeIdentity || session?.state === 'archived' }, { key: 'schedules', icon: <CalendarOutlined />, label: '定时任务', disabled: !session || session.state === 'archived' }, { key: 'copy', icon: <CopyOutlined />, label: '复制会话 ID', disabled: !id }], onClick: ({ key }: { key: string }) => {
    if (key === 'rename' || key === 'native' || key === 'archive' || key === 'control') setSessionDialog(key)
    if (key === 'schedules' && session) onSchedules(session)
    if (key === 'copy' && id) void navigator.clipboard.writeText(id).catch(() => onDraft((old) => ({ ...old, error: `复制失败，会话 ID：${id}` })))
  } }
  const lastReceipt = draft.pending.at(-1)
  const receiptFailed = !!lastReceipt && ['failed', 'orphaned', 'unknown', 'rejected', 'expired', 'unconfirmed'].includes(lastReceipt.state)
  const receiptError = receiptFailed ? codexOperationError(lastReceipt?.detail) : undefined
  const prompt = (text: string) => { onDraft((old) => old.text.trim() ? old : { ...old, text }); sender.current?.focus({ preventScroll: true, cursor: 'end' }) }
  const submit = () => {
    if (session?.state === 'archived') return
    // An explicit send returns to the latest turn; background deltas keep the reading anchor.
    follow.current = true; anchor.current = undefined; anchorLocked.current = false
    setAway(false); setNewMessages(false)
    onSend(visibleTurn, !!agent?.capabilities?.includes('codex.native_completion') && page?.context?.ephemeral === false); sender.current?.focus({ preventScroll: true })
  }
  return <section className={`codex-conversation ${!id ? 'conversation-unselected' : ''}`}>
    {modalHolder}
    {session && sessionDialog === 'archive' && <ArchiveDialog session={session} csrf={csrf} onClose={() => setSessionDialog(undefined)} />}
    {session && sessionDialog === 'rename' && <RenameSession session={session} csrf={csrf} onClose={() => setSessionDialog(undefined)} />}
    {session && sessionDialog === 'native' && <NativeSession session={session} csrf={csrf} onClose={() => setSessionDialog(undefined)} onRename={() => setSessionDialog('rename')} />}
    {session && <NativeControlDrawer supportsCompletion={!!agent?.capabilities?.includes('codex.native_completion')} csrf={csrf} session={session.session_id} turns={turns} activeTurn={session.active_turn_id} open={sessionDialog === 'control'} onClose={() => setSessionDialog(undefined)} />}
    <header className="conversation-head">
      <div className="conversation-identity">
        <Button className="mobile-only" type="text" icon={<MenuOutlined />} onClick={onRail} aria-label="打开会话列表" />
        <Tooltip title={collapsed ? '展开会话列表' : '收起会话列表'}><Button className="desktop-only" type="text" icon={collapsed ? <MenuUnfoldOutlined /> : <MenuFoldOutlined />} onClick={onCollapse} aria-label={collapsed ? '展开会话列表' : '折叠会话列表'} /></Tooltip>
        <div className="conversation-title"><div className="conversation-meta">{session ? <><FolderOpenOutlined /><span>{session.project_id}</span><span className="context-separator">/</span><span>{agent?.hostname ?? session.agent_id}</span></> : '你的项目，你的工作进度'}</div><h1>{session ? sessionName(session) : id ? '正在读取会话…' : 'Codex 工作区'}</h1></div>
      </div>
      <Space size={4}><Tooltip title="刷新对话"><Button type="text" icon={<ReloadOutlined />} disabled={!id} loading={history.isFetching} onClick={() => void load()} aria-label="刷新对话" /></Tooltip><Dropdown menu={menu} trigger={['click']}><Button type="text" icon={<EllipsisOutlined />} aria-label="会话操作" /></Dropdown></Space>
    </header>
    <div className="conversation-alerts">{hidden && <Alert type="info" title="当前会话所属项目已隐藏，草稿已保留" action={<Button disabled={projects.save.isPending} onClick={() => projects.save.mutate([{ ...projects.preference(project), hidden: false }])}>恢复项目显示</Button>} />}{projects.save.error && <Alert type="warning" title={errorText(projects.save.error)} />}{session?.state === 'archived' && <Alert type="info" title="会话已归档，恢复后可继续对话" action={<Button onClick={() => setSessionDialog('archive')}>恢复会话</Button>} />}{codexNotice && <Alert showIcon type={codex?.state === 'starting' ? 'info' : 'warning'} title={codexNotice} />}{draft.error && <Alert showIcon closable type="warning" title="指令尚未完成提交" description={draft.error} onClose={() => onDraft((old) => ({ ...old, error: undefined }))} />}{receiptError && <Alert showIcon type="warning" title="指令执行失败" description={receiptError} />}{failure && turns.length > 0 && <Alert showIcon type="warning" title="历史刷新失败，已保留当前内容" description={failure.message} action={<Button onClick={() => void load(!!moreError)}>重试</Button>} />}</div>
    <div className="conversation-history"><div className="conversation-scroll" ref={bindScroll} onWheel={() => { anchorLocked.current = false }} onTouchMove={() => { anchorLocked.current = false }} onScroll={() => {
      const node = scroll.current; if (!node || restoring.current || anchorLocked.current) return
      // A delayed scroll event from our own correction is not a new reading
      // position. Keep the original message anchor through later measurements.
      if (restoredTop.current !== undefined && Math.abs(node.scrollTop - restoredTop.current) < 1) return
      restoredTop.current = undefined
      follow.current = node.scrollHeight - node.scrollTop - node.clientHeight < 80; setAway(!follow.current); if (follow.current) setNewMessages(false); capture()
    }}>
      {!id ? <Welcome recent={recent} hasDraft={false} onNew={onNew} onBrowse={onBrowse} onSelect={onSelect} onPrompt={prompt} /> : loading && !page ? <div className="history-loading" role="status"><span>正在读取对话历史…</span><Skeleton active paragraph={{ rows: 5 }} /></div> : failure && !turns.length ? <Empty className="codex-empty" description={<><p>{unavailable}</p><p className="conversation-meta">{failure.message}</p></>}><Button onClick={() => void load()} icon={<ReloadOutlined />}>重试读取</Button></Empty> : page && !turns.length && session ? <Welcome session={session} recent={[]} hasDraft={!!draft.text.trim()} onNew={onNew} onBrowse={onBrowse} onSelect={onSelect} onPrompt={prompt} /> : null}
      {page?.next_cursor && !needsMessage && <Button loading={loading} className="load-earlier" onClick={() => void load(true)}>加载更早对话</Button>}
      <Transcript session={id ?? ''} turns={turns} scroll={scrollElement} position={position} continuation={needsMessage ? continuation : undefined} loading={loading} onContinue={continueHistory} onAnchor={anchorInteraction} />
      {session && <NativeActivity session={session.session_id} enabled={!!agent?.online && !!agent.capabilities?.includes('codex.native_completion')} />}
      {session && <NativeInteractionCards csrf={csrf} session={session.session_id} enabled={!!agent?.online && !!agent.capabilities?.includes('codex.interactions')} />}
      <div className="pending-messages">{pending.map((p) => <article key={p.id} className={`codex-message user pending-message ${p.state === 'submitting' ? 'is-submitting' : ''}`} data-message-key={`pending:${p.id}`}><div className="message-role">{p.state === 'submitting' ? <LoadingOutlined /> : ['failed', 'orphaned', 'unknown', 'rejected', 'expired', 'unconfirmed'].includes(p.state) ? <CloseCircleOutlined /> : p.command_id ? <CheckCircleOutlined /> : <ClockCircleOutlined />} 你 · {stateNames[p.state] ?? '处理中'}</div><div className="message-body user-text">{p.text}</div></article>)}</div>
      {visibleTurn && <div className="response-activity" role="status"><ActivityIndicator /><span>{turns.some((turn) => turn.items.some((item) => item.streaming)) ? 'Codex 正在回复' : 'Codex 正在处理'}<small>你可以继续编写下一条指令</small></span></div>}
    </div><Button className="jump-to-bottom" icon={<ArrowDownOutlined aria-hidden />} hidden={!away && !newMessages} onClick={() => { follow.current = true; anchor.current = undefined; restore(); setNewMessages(false); setAway(false) }}>{newMessages ? '有新消息 · 回到底部' : '回到底部'}</Button></div>
    {id && <footer className="composer-wrap">
      <NativeComposer csrf={csrf} session={id} draft={draft} onDraft={onDraft} enabled={!!agent?.capabilities?.includes('codex.interactions')} disabled={draft.sending || !session || session.state === 'archived' || !agent?.online}>
      <Sender ref={sender} className={`composer ${draft.text.trim() ? 'has-draft' : ''} ${draft.sending ? 'is-submitting' : ''}`} components={senderComponents} value={draft.text} onChange={(value) => onDraft((old) => ({ ...old, text: value }))} autoSize={{ minRows: 1, maxRows: 5 }} placeholder={session ? '描述下一步，或提出问题…' : '正在读取会话…'} disabled={!session || session.state === 'archived' || failureCode === 'session_not_found'} onSubmit={submit} onKeyDown={(event) => { if (event.nativeEvent.isComposing || event.keyCode === 229) return false }} suffix={false} footer={(_, { components: { SendButton } }) => <>
        {visibleTurn && <div className="active-turn-controls"><Radio.Group className="delivery-options" aria-label="活动会话发送方式" value={draft.delivery} onChange={(event) => onDraft((old) => ({ ...old, delivery: event.target.value as 'queue' | 'steer' }))}><Radio value="queue">排队下一轮</Radio><Radio value="steer">补充当前对话</Radio></Radio.Group><Tooltip title="中断当前对话"><Button danger type="text" icon={<StopOutlined />} onClick={interrupt} aria-label="中断" /></Tooltip></div>}
        <div className="composer-actions">
          <div className="composer-tools"><PermissionDetails context={page?.context} session={session} supported={agent ? agent.capabilities?.includes('codex.session_context') ?? false : undefined} /><Tooltip title="定时发送"><Button type="text" className="composer-control schedule-action" icon={<ClockCircleOutlined />} disabled={!session || session.state === 'archived'} onClick={() => session && onSchedule(session)} aria-label="定时发送" /></Tooltip></div>
          <div className="composer-submit"><ModelPicker context={page?.context} session={session} choice={draft.model_choice} sending={draft.sending} steer={!!visibleTurn && draft.delivery === 'steer'} supported={!!agent?.capabilities?.includes('codex.model_choice')} onChange={choice => onDraft(old => ({ ...old, model_choice: choice }))} />
          <SendButton className="send-action" shape="default" type="primary" icon={<ArrowUpOutlined />} loading={draft.sending} disabled={!session || session.state === 'archived' || !draft.text.trim() || draft.sending || draft.images?.some(image => image.state !== 'ready')} aria-label="发送指令"><span className="sr-only">发送</span></SendButton>
          </div>
        </div>
      </>} />
      </NativeComposer>
      <div className="composer-hint"><span className={`submission-status ${receiptFailed ? 'status-error' : ''}`} role="status">{draft.sending ? <><LoadingOutlined /> 等待 Agent 保存确认…</> : lastReceipt?.command_id ? <>{receiptFailed ? <CloseCircleOutlined /> : ['accepted', 'completed'].includes(lastReceipt.state) ? <CheckCircleOutlined /> : <ClockCircleOutlined />} {stateNames[lastReceipt.state] ?? '处理中'}<span className="receipt-id"> · {lastReceipt.command_id.slice(-8)}</span></> : session ? <><FolderOpenOutlined /> {session.project_id}</> : '正在读取会话'}</span><span className="keyboard-hint"><kbd>Enter</kbd> 发送 <span>·</span> <kbd>Shift + Enter</kbd> 换行</span></div>
    </footer>}

  </section>
}
