import { ArrowLeftOutlined, CalendarOutlined, ClockCircleOutlined, DownOutlined, FolderOpenOutlined, MenuOutlined, PlusOutlined, ReloadOutlined, SearchOutlined, SendOutlined, StopOutlined } from '@ant-design/icons'
import { Alert, Badge, Button, Collapse, Drawer, Empty, Form, Input, List, Modal, Radio, Segmented, Select, Space, Spin, Tag, Typography } from 'antd'
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'

import { subscribeEvents } from '../api/events'
import { mergeTurns } from '../api/transcript'
import { cancelSchedule, createSchedule, createSession, fetchExperiments, fetchProjects, fetchSchedules, fetchSessionPage, fetchTranscript, interruptSession, sendMessage, waitForCommand, json, type Operation, type CodexSchedule, type CodexSession, type Experiment, type ProjectCandidate, type TranscriptTurn } from '../api/features'

type ArchiveFilter = 'false' | 'true' | 'all'
type Delivery = 'at_time' | 'experiment_succeeded'

function stateColor(state: CodexSession['state']) {
  if (state === 'running') return '#22C7A9'
  if (state === 'failed' || state === 'orphaned') return '#ff7875'
  if (state === 'queued' || state === 'creating') return '#faad14'
  return '#7f8c99'
}

function Transcript({ turns, live }: { turns: TranscriptTurn[]; live: string }) {
  if (turns.length === 0 && !live) return <Empty className="codex-empty" description="这个会话还没有可显示的对话" />
  return <div className="codex-transcript" aria-live="polite">
    {turns.flatMap((turn) => turn.items.map((item) => {
      if (item.kind === 'command_summary' || item.kind === 'file_change_summary') return <Collapse key={item.item_id} ghost size="small" className="execution-summary" items={[{ key: 'summary', label: item.kind === 'command_summary' ? '命令执行摘要' : '文件变更摘要', children: <pre>{item.text}</pre> }]} />
      const role = item.kind === 'user_message' ? 'user' : item.kind === 'error' ? 'error' : 'assistant'
      return <article key={item.item_id} className={`codex-message ${role}`}><div className="message-role">{role === 'user' ? '你' : role === 'error' ? '错误' : 'Codex'}</div><div className="message-body">{item.text}</div></article>
    }))}
    {live && <article className="codex-message assistant streaming"><div className="message-role">Codex</div><div className="message-body">{live}<span className="stream-caret" /></div></article>}
  </div>
}

export function CodexPage({ csrf }: { csrf: string }) {
  const navigate = useNavigate(); const [searchParams] = useSearchParams(); const targetSession = searchParams.get('session')
  const sessionsGeneration = useRef(0); const [sessionCursor, setSessionCursor] = useState<string>(); const [operation, setOperation] = useState<Operation>(); const [delivery, setDelivery] = useState<'queue' | 'steer'>('queue'); const sendLock = useRef(false); const [sessions, setSessions] = useState<CodexSession[]>([]); const [selectedId, setSelectedId] = useState<string | undefined>(targetSession ?? undefined)
  const [archiveFilter, setArchiveFilter] = useState<ArchiveFilter>('false'); const [query, setQuery] = useState(''); const [railOpen, setRailOpen] = useState(false); const [railCollapsed, setRailCollapsed] = useState(false)
  const [creating, setCreating] = useState(false); const [scheduling, setScheduling] = useState(false); const [scheduleListOpen, setScheduleListOpen] = useState(false)
  const [projects, setProjects] = useState<ProjectCandidate[]>([]); const [experiments, setExperiments] = useState<Experiment[]>([]); const [schedules, setSchedules] = useState<CodexSchedule[]>([])
  const [turns, setTurns] = useState<TranscriptTurn[]>([]); const [cursor, setCursor] = useState<string>(); const [historyLoading, setHistoryLoading] = useState(false); const [live, setLive] = useState<Record<string, string>>({}); const scroll = useRef<HTMLDivElement>(null); const anchor = useRef<{ height: number; top: number } | null>(null); const follow = useRef(true)
  const drafts = useRef(new Map<string, string>()); const [prompt, setPrompt] = useState(''); const [error, setError] = useState<string>(); const [sending, setSending] = useState(false); const requestGeneration = useRef(0)

  const loadSessions = useCallback(async () => { const generation = ++sessionsGeneration.current; try { const page = await fetchSessionPage(undefined, archiveFilter); if (targetSession && !page.sessions.some((item) => item.session_id === targetSession)) { try { page.sessions.push(await json<CodexSession>(`/api/v1/codex/sessions/${encodeURIComponent(targetSession)}`)) } catch { /* The history request reports unavailable targets. */ } } if (generation !== sessionsGeneration.current) return; setSessions((old) => [...page.sessions, ...old.filter((item) => !page.sessions.some((fresh) => fresh.session_id === item.session_id))]); setSessionCursor(page.next_cursor); setSelectedId((current) => current ?? page.sessions[0]?.session_id) } catch (reason) { setError(reason instanceof Error ? reason.message : 'Codex 会话不可用') } }, [archiveFilter, targetSession])
  const moreSessions = async () => { try { const page = await fetchSessionPage(undefined, archiveFilter, sessionCursor); setSessions((old) => [...old, ...page.sessions.filter((item) => !old.some((existing) => existing.session_id === item.session_id))]); setSessionCursor(page.next_cursor) } catch (reason) { setError(reason instanceof Error ? reason.message : '会话加载失败') } }
  // Initial remote synchronization; state updates happen after the requests settle.
  useEffect(() => { const initial = setTimeout(() => { void loadSessions(); void fetchProjects().then(setProjects).catch((e: unknown) => setError(String(e))); void fetchExperiments().then(setExperiments).catch((e: unknown) => setError(String(e))) }, 0); return () => clearTimeout(initial) }, [loadSessions])
  const selected = sessions.find((item) => item.session_id === selectedId)

  const loadHistory = useCallback(async (older = false) => {
    if (!selectedId) return; if (older && scroll.current) anchor.current = { height: scroll.current.scrollHeight, top: scroll.current.scrollTop }; const generation = older ? requestGeneration.current : ++requestGeneration.current; setHistoryLoading(true)
    try { const page = await fetchTranscript(selectedId, older ? cursor : undefined); if (generation !== requestGeneration.current) return; const chronological = [...page.turns].reverse(); setTurns((current) => mergeTurns(current, chronological, older)); setCursor(page.next_cursor); if (!older) setLive({}) }
    catch (reason) { if (generation === requestGeneration.current) setError(reason instanceof Error ? reason.message : '无法读取对话历史') }
    finally { if (generation === requestGeneration.current) setHistoryLoading(false) }
  }, [cursor, selectedId])

  // Reset transient presentation state when the selected remote thread changes.
  useEffect(() => { ++requestGeneration.current; follow.current = true; setPrompt(drafts.current.get(selectedId ?? '') ?? ''); setOperation(undefined); setTurns([]); setCursor(undefined); setLive({}); setSchedules([]); if (selectedId) void loadHistory(false) }, [selectedId]) // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => { let active = true; if (selectedId) void fetchSchedules(selectedId).then((value) => { if (active) setSchedules(value) }).catch((e: unknown) => setError(String(e))); return () => { active = false } }, [selectedId])
  useEffect(() => {
    let active = true
    const off = subscribeEvents(['open', 'codex.session.updated', 'codex.schedule.updated', 'codex.turn.completed', 'codex.turn.failed', 'codex.turn.orphaned', 'codex.stream.resync', 'codex.message.delta'], (event) => {
      if (event.type === 'open' || event.type === 'codex.stream.resync') { void loadHistory(false); return }
      let payload: { session_id?: string; data?: { delta?: string; turn_id?: string; item_id?: string } } = {}
      try { payload = (JSON.parse(event.data) as { payload?: typeof payload }).payload ?? {} } catch { return }
      if (event.type === 'codex.session.updated') void loadSessions()
      if (payload.session_id !== selectedId) return
      if (event.type === 'codex.schedule.updated') void fetchSchedules(selectedId).then((value) => { if (active) setSchedules(value) }).catch((e: unknown) => setError(String(e)))
      if (event.type.startsWith('codex.turn.')) { void loadSessions(); void loadHistory(false) }
      if (event.type === 'codex.message.delta' && payload.data?.delta) setLive((value) => { const key = `${payload.data!.turn_id ?? 'active'}:${payload.data!.item_id ?? 'assistant'}`; return { ...value, [key]: (value[key] ?? '') + payload.data!.delta } })
    })
    return () => { active = false; off() }
  }, [loadHistory, loadSessions, selectedId])
  useEffect(() => {
    if (!operation?.command_id || ['completed', 'failed', 'expired'].includes(operation.state ?? '')) return
    let active = true
    const timer = setInterval(() => { void json<Operation>(`/api/v1/commands/${operation.command_id}`).then((status) => { if (active) { setOperation(status); if (['failed', 'expired'].includes(status.state ?? '')) setError(`指令结果：${status.state}`) } }).catch((e: unknown) => { if (active) setError(String(e)) }) }, 1500)
    return () => { active = false; clearInterval(timer) }
  }, [operation?.command_id, operation?.state])

  useLayoutEffect(() => {
    const node = scroll.current; if (!node) return
    if (anchor.current) { node.scrollTop = anchor.current.top + node.scrollHeight - anchor.current.height; anchor.current = null }
    else if (follow.current) node.scrollTop = node.scrollHeight
  }, [turns, live])

  const grouped = useMemo(() => { const filtered = sessions.filter((item) => `${item.title ?? ''} ${item.project_id}`.toLowerCase().includes(query.toLowerCase())); return Object.entries(filtered.reduce<Record<string, CodexSession[]>>((groups, item) => { (groups[`${item.agent_id} / ${item.project_id}`] ??= []).push(item); return groups }, {})) }, [query, sessions])
  const send = async () => {
    if (!selected || !prompt.trim() || sendLock.current) return
    const text = prompt.trim(); const target = selected.session_id; const generation = requestGeneration.current; sendLock.current = true; setSending(true); setError(undefined)
    try { const receipt = await sendMessage(csrf, target, text, selected.state === 'running' ? delivery : 'queue'); if (drafts.current.get(target)?.trim() === text) drafts.current.delete(target); if (generation === requestGeneration.current) { setOperation(receipt); setPrompt((current) => current.trim() === text ? '' : current); } await loadSessions() }
    catch (reason) { setError(reason instanceof Error ? reason.message : '发送失败，草稿已保留') }
    finally { sendLock.current = false; setSending(false) }
  }
  const interrupt = () => { if (!selected?.active_turn_id) return; const target = selected; Modal.confirm({ title: '中断当前 Codex 对话？', content: `会话 ${target.session_id}，turn ${target.active_turn_id}。已执行的修改不会撤销。`, onOk: async () => { try { await waitForCommand(await interruptSession(csrf, target.session_id, target.active_turn_id)); await loadSessions() } catch (e) { setError(e instanceof Error ? e.message : '中断失败'); throw e } } }) }


  const rail = <aside className="codex-rail" aria-label="项目和会话"><div className="codex-rail-head"><Button type="text" icon={<ArrowLeftOutlined />} onClick={() => navigate('/')} aria-label="返回控制台" /><Typography.Title level={1} style={{ fontSize: 18, margin: 0 }}>Codex</Typography.Title><Button type="text" icon={<PlusOutlined />} onClick={() => setCreating(true)} aria-label="新建会话" /></div><Input allowClear prefix={<SearchOutlined />} placeholder="搜索项目或会话" value={query} onChange={(event) => setQuery(event.target.value)} /><Segmented block value={archiveFilter} onChange={(value) => { setSessions([]); setSelectedId(undefined); setArchiveFilter(value as ArchiveFilter) }} options={[{ label: '当前', value: 'false' }, { label: '归档', value: 'true' }, { label: '全部', value: 'all' }]} /><div className="codex-session-groups">{grouped.map(([project, items]) => <section key={project} className="session-group"><div className="session-group-title"><FolderOpenOutlined /><span>{project}</span></div><List dataSource={items} renderItem={(item) => <List.Item className={selectedId === item.session_id ? 'session-row selected' : 'session-row'}><Button type="text" className="session-select" block aria-pressed={selectedId === item.session_id} onClick={() => { setSelectedId(item.session_id); setRailOpen(false) }}><div className="session-copy"><span>{item.title || '未命名会话'}</span><small>{item.mode} · {item.state}</small></div><Badge color={stateColor(item.state)} /></Button></List.Item>} /></section>)}{sessionCursor && <Button onClick={() => void moreSessions()}>加载更多会话</Button>}</div></aside>

  return <main className={`codex-workspace ${railCollapsed ? 'rail-collapsed' : ''}`}><div className="codex-desktop-rail">{rail}</div><Drawer className="codex-mobile-drawer" placement="left" open={railOpen} onClose={() => setRailOpen(false)} width={Math.min(320, window.innerWidth - 32)} closable={false}>{rail}</Drawer><section className="codex-conversation"><header className="conversation-head"><Space><Button className="mobile-only" type="text" icon={<MenuOutlined />} onClick={() => setRailOpen(true)} aria-label="打开会话列表" /><Button className="desktop-only" type="text" icon={railCollapsed ? <MenuOutlined /> : <ArrowLeftOutlined />} onClick={() => setRailCollapsed((value) => !value)} aria-label="折叠会话列表" /><div><Typography.Text strong>{selected ? selected.title || selected.session_id : '选择一个会话'}</Typography.Text><div className="conversation-meta">{selected ? `${selected.project_id} · ${selected.agent_id} · ${selected.mode}` : 'FarHelm Codex workspace'}</div></div></Space><Space><Button icon={<CalendarOutlined />} onClick={() => setScheduleListOpen(true)}>定时任务</Button><Button icon={<ReloadOutlined />} onClick={() => void loadHistory(false)} aria-label="刷新对话" /></Space></header>{error && <Alert className="codex-alert" showIcon closable type="warning" title="Codex 操作失败" description={error} onClose={() => setError(undefined)} />}<div className="conversation-scroll" ref={scroll} onScroll={() => { if (scroll.current) follow.current = scroll.current.scrollHeight - scroll.current.scrollTop - scroll.current.clientHeight < 80 }}>{cursor && <Button loading={historyLoading} className="load-earlier" icon={<DownOutlined rotate={180} />} onClick={() => void loadHistory(true)}>加载更早内容</Button>}{historyLoading && turns.length === 0 ? <Spin /> : <Transcript turns={turns} live={Object.values(live).join('\n')} />}</div><footer className="composer-wrap">{operation?.command_id && <Typography.Text type="secondary">Agent 指令：{operation.state} · {operation.command_id}</Typography.Text>}<div className="composer"><Input.TextArea aria-label="给 Codex 发送指令" value={prompt} onChange={(event) => { setPrompt(event.target.value); if (selectedId) drafts.current.set(selectedId, event.target.value) }} autoSize={{ minRows: 2, maxRows: 8 }} placeholder={selected ? '给 Codex 发送指令…' : '请先选择会话'} disabled={!selected} onPressEnter={(event) => { if (!event.shiftKey && !event.nativeEvent.isComposing && event.keyCode !== 229) { event.preventDefault(); void send() } }} /><div className="composer-actions">{selected?.state === 'running' && <Radio.Group aria-label="活动会话发送方式" value={delivery} onChange={(event) => setDelivery(event.target.value as 'queue' | 'steer')}><Radio value="queue">排队</Radio><Radio value="steer">补充当前对话</Radio></Radio.Group>}<Space><Button type="text" icon={<ClockCircleOutlined />} disabled={!selected} onClick={() => setScheduling(true)}>定时发送</Button>{selected?.state === 'running' && <Button danger type="text" icon={<StopOutlined />} onClick={interrupt}>中断</Button>}</Space><Button shape="circle" type="primary" icon={<SendOutlined />} loading={sending} disabled={!selected || !prompt.trim()} onClick={() => void send()} aria-label="发送指令" /></div></div></footer></section>

    <Modal title="创建 Codex 会话" open={creating} onCancel={() => setCreating(false)} footer={null}><Form layout="vertical" onFinish={(values: { project: string; mode: 'inspect' | 'edit' }) => { const project = projects.find((item) => item.candidate_id === values.project && item.state === 'approved'); if (!project) return; void createSession(csrf, project.agent_id, project.suggested_project_id, values.mode).then(waitForCommand).then(() => { setCreating(false); void loadSessions() }).catch((reason: unknown) => setError(reason instanceof Error ? reason.message : '创建失败')) }}><Form.Item label="项目" name="project" rules={[{ required: true }]}><Select options={projects.filter((item) => item.state === 'approved').map((item) => ({ value: item.candidate_id, label: `${item.display_name} · ${item.agent_id}` }))} /></Form.Item><Form.Item label="模式" name="mode" initialValue="inspect"><Radio.Group><Radio value="inspect">Inspect（只读）</Radio><Radio value="edit">Edit（隔离 worktree）</Radio></Radio.Group></Form.Item><Button type="primary" htmlType="submit" block>创建</Button></Form></Modal>
    <Modal title="定时发送" open={scheduling} onCancel={() => setScheduling(false)} footer={null}><Form layout="vertical" initialValues={{ delivery: 'at_time', prompt }} onFinish={(values: { delivery: Delivery; prompt: string; runAt?: string; watchId?: string }) => { if (!selected) return; const trigger = values.delivery === 'at_time' ? { type: 'at_time' as const, run_at_unix: Math.floor(new Date(values.runAt!).getTime() / 1000) } : { type: 'experiment_succeeded' as const, watch_id: values.watchId! }; void createSchedule(csrf, selected.session_id, values.prompt, trigger).then(waitForCommand).then(() => { setScheduling(false); setPrompt(''); void fetchSchedules(selected.session_id).then(setSchedules).catch((reason: unknown) => setError(String(reason))) }).catch((reason: unknown) => setError(reason instanceof Error ? reason.message : '创建定时任务失败')) }}><Form.Item name="delivery" label="触发方式"><Segmented block options={[{ label: '指定时间', value: 'at_time' }, { label: '训练成功后', value: 'experiment_succeeded' }]} /></Form.Item><Form.Item noStyle shouldUpdate>{({ getFieldValue }) => getFieldValue('delivery') === 'at_time' ? <Form.Item name="runAt" label="发送时间（本地时区）" rules={[{ required: true }]}><Input type="datetime-local" /></Form.Item> : <Form.Item name="watchId" label="训练任务" rules={[{ required: true }]}><Select options={experiments.filter((item) => item.state === 'watching' && item.agent_id === selected?.agent_id && item.project_id === selected?.project_id).map((item) => ({ value: item.watch_id, label: item.name }))} /></Form.Item>}</Form.Item><Form.Item name="prompt" label="指令" rules={[{ required: true }, { max: 32768 }]}><Input.TextArea autoSize={{ minRows: 4, maxRows: 10 }} /></Form.Item><Button type="primary" htmlType="submit" block>创建定时任务</Button></Form></Modal>
    <Drawer title="定时任务" open={scheduleListOpen} onClose={() => setScheduleListOpen(false)}><List locale={{ emptyText: '当前会话没有定时任务' }} dataSource={schedules} renderItem={(item) => <List.Item actions={['pending', 'queued'].includes(item.state) ? [<Button key="cancel" danger type="link" onClick={() => void cancelSchedule(csrf, item.schedule_id).then(waitForCommand).then(() => { if (selectedId) return fetchSchedules(selectedId).then(setSchedules) }).catch((e: unknown) => setError(String(e)))}>取消</Button>] : undefined}><List.Item.Meta title={item.trigger.type === 'at_time' ? new Date(item.trigger.run_at_unix * 1000).toLocaleString() : '训练成功后'} description={<Space><Tag>{item.state}</Tag><Typography.Text type="secondary">{item.trigger.type === 'experiment_succeeded' ? item.trigger.watch_id : item.schedule_id}</Typography.Text></Space>} /></List.Item>} /></Drawer>
  </main>
}
