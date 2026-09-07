import { ArrowLeftOutlined, CheckCircleOutlined, ClockCircleOutlined, CloseCircleOutlined, FolderOpenOutlined, LoadingOutlined, PlusOutlined, SearchOutlined } from '@ant-design/icons'
import { Alert, Button, Drawer, Empty, Input, Segmented, Select, Skeleton } from 'antd'
import { useEffect, useMemo, useState } from 'react'
import { Conversations } from '@ant-design/x'
import { useQuery } from '@tanstack/react-query'
import { flushSync } from 'react-dom'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { fetchProjects, type CodexSession } from '../api/features'
import { Conversation } from './codex/Conversation'
import { CreateDialog, ScheduleDialog, SchedulesDrawer } from './codex/Dialogs'
import { errorText, sessionName, stateNames } from './codex/presentation'
import { useOperations } from './codex/useOperations'
import { useSessions, type ArchiveFilter } from './codex/useSessions'

function StateIcon({ state }: { state: string }) {
  return state === 'running' ? <LoadingOutlined /> : ['failed', 'orphaned'].includes(state) ? <CloseCircleOutlined /> : ['queued', 'creating'].includes(state) ? <ClockCircleOutlined /> : <CheckCircleOutlined />
}
export function CodexPage({ csrf }: { csrf: string }) {
  const navigate = useNavigate(); const [params, setParams] = useSearchParams(); const id = params.get('session') ?? undefined
  const [closedGroups, setClosedGroups] = useState<Record<string, boolean>>({})
  const [query, setQuery] = useState(''); const [archive, setArchive] = useState<ArchiveFilter>('false')
  const [scope, setScope] = useState<string>(); const [railOpen, setRailOpen] = useState(false); const [collapsed, setCollapsed] = useState(false)
  const projectQuery = useQuery({ queryKey: ['projects'], queryFn: fetchProjects }); const projects = useMemo(() => projectQuery.data ?? [], [projectQuery.data]); const projectError = projectQuery.error ? errorText(projectQuery.error) : undefined
  const [creating, setCreating] = useState(false); const [schedule, setSchedule] = useState<CodexSession>(); const [schedules, setSchedules] = useState<CodexSession>()
  const scopes = useMemo(() => {
    const agents = [...new Set(projects.map((p) => p.agent_id))]
    return agents.flatMap((agent_id) => [{ value: JSON.stringify([agent_id]), label: `${agent_id} / 全部项目`, agent_id, project_id: undefined as string | undefined }, ...projects.filter((p) => p.agent_id === agent_id && p.state === 'approved').map((p) => ({ value: JSON.stringify([agent_id, p.suggested_project_id]), label: `${agent_id} / ${p.display_name}`, agent_id, project_id: p.suggested_project_id }))])
  }, [projects])
  const scoped = scopes.find((option) => option.value === scope)
  const sessions = useSessions(csrf, query, archive, scoped?.agent_id, scoped?.project_id)
  const operations = useOperations(csrf)
  useEffect(() => {
    const viewport = window.visualViewport
    const resize = () => { document.documentElement.style.setProperty('--codex-height', `${viewport?.height ?? innerHeight}px`); document.documentElement.style.setProperty('--codex-top', `${viewport?.offsetTop ?? 0}px`) }
    resize(); viewport?.addEventListener('resize', resize); viewport?.addEventListener('scroll', resize); window.addEventListener('resize', resize)
    return () => { viewport?.removeEventListener('resize', resize); viewport?.removeEventListener('scroll', resize); window.removeEventListener('resize', resize); document.documentElement.style.removeProperty('--codex-height'); document.documentElement.style.removeProperty('--codex-top') }
  }, [])
  const groups = useMemo(() => {
    const groups = new Map<string, CodexSession[]>()
    for (const session of sessions.rows) { const key = `${session.agent_id} / ${session.project_id}`; groups.set(key, [...(groups.get(key) ?? []), session]) }
    return [...groups]
  }, [sessions.rows])
  const select = (sessionId?: string) => { const next = new URLSearchParams(params); if (sessionId) next.set('session', sessionId); else next.delete('session'); flushSync(() => setParams(next)); setRailOpen(false) }
  const rail = <aside className="codex-rail" aria-label="项目和会话"><div className="codex-rail-head"><Button type="text" icon={<ArrowLeftOutlined />} onClick={() => navigate('/')} aria-label="返回控制台" /><h1>Codex</h1><Button type="text" icon={<PlusOutlined />} onClick={() => { setCreating(true); setRailOpen(false) }} aria-label="新建会话" /></div>
    <Input allowClear prefix={<SearchOutlined />} placeholder="搜索全部会话" aria-label="搜索全部会话" value={query} onChange={(e) => setQuery(e.target.value)} />
    <Segmented block value={archive} onChange={(value) => { setArchive(value as ArchiveFilter); select(undefined) }} options={[{ label: '当前', value: 'false' }, { label: '归档', value: 'true' }, { label: '全部', value: 'all' }]} />
    {projects.length > 0 && <Select aria-label="筛选项目" allowClear placeholder="全部服务器与项目" value={scope} onChange={setScope} options={scopes} />}
    {(sessions.error || projectError) && <Alert type="warning" title={sessions.error ?? projectError} action={<Button type="text" onClick={() => { void sessions.refresh(); if (projectError) void projectQuery.refetch() }}>重试</Button>} />}
    {sessions.incomplete.length > 0 && <div className="session-partial" role="status">部分会话信息不可用：{sessions.incomplete.map((a) => `${a.agent_id}（${a.reason === 'agent_offline' ? '离线' : a.reason === 'agent_upgrade_required' ? '需要升级 Agent' : '读取失败'}）`).join('、')}<Button type="link" onClick={() => void sessions.refresh()}>重试</Button></div>}
    <div className="codex-session-groups" onKeyDown={(event) => {
      if (['Enter', ' '].includes(event.key) && (event.target as HTMLElement).classList.contains('session-select')) { event.preventDefault(); (event.target as HTMLElement).click(); return }
      if (!['ArrowUp', 'ArrowDown', 'Home', 'End'].includes(event.key)) return
      const buttons = [...event.currentTarget.querySelectorAll<HTMLButtonElement>('.session-select')]; const index = buttons.indexOf(document.activeElement as HTMLButtonElement); if (index < 0) return
      event.preventDefault(); const next = event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1 : Math.max(0, Math.min(buttons.length - 1, index + (event.key === 'ArrowDown' ? 1 : -1))); buttons[next]?.focus()
    }}>{sessions.loading && !sessions.rows.length ? <Skeleton active paragraph={{ rows: 6 }} /> : groups.length ? <Conversations className="session-conversations" activeKey={id} onActiveChange={select} groupable={{ collapsible: true, expandedKeys: groups.filter(([group]) => !closedGroups[group]).map(([group]) => group), onExpand: (keys) => setClosedGroups((old) => ({ ...old, ...Object.fromEntries(groups.map(([group]) => [group, !keys.includes(group)])) })), label: (group) => { const rows = groups.find(([key]) => key === group)![1]; return <Button type="text" className="session-group-heading" aria-expanded={!closedGroups[group]} onClick={(event) => { event.stopPropagation(); setClosedGroups((old) => ({ ...old, [group]: !old[group] })) }}><FolderOpenOutlined /><span><strong>{rows[0].project_id}</strong><small>{rows[0].agent_id} · {rows.length} 个会话</small></span></Button> } }} items={sessions.rows.map((s) => ({ key: s.session_id, group: `${s.agent_id} / ${s.project_id}`, className: 'session-row', label: <Button type="text" className="session-select" block aria-label={sessionName(s)} aria-current={s.session_id === id ? 'page' : undefined} aria-pressed={s.session_id === id}><span className="session-copy"><span title={sessionName(s)}>{sessionName(s)}</span>{['running', 'queued', 'creating', 'failed', 'orphaned'].includes(s.state) && <small><StateIcon state={s.state} /> {stateNames[s.state]}</small>}</span></Button> }))} /> : !sessions.error && <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={query ? '当前范围内没有匹配的会话' : '当前范围内没有会话'} />}{sessions.cursor && <Button block loading={sessions.loading} onClick={() => void sessions.more()}>加载更多会话</Button>}</div>
  </aside>
  return <main className={`codex-workspace ${collapsed ? 'rail-collapsed' : ''}`}><div className="codex-desktop-rail">{rail}</div><Drawer className="codex-mobile-drawer" placement="left" open={railOpen} onClose={() => setRailOpen(false)} size={Math.min(320, window.innerWidth - 24)} closable={false}>{rail}</Drawer>
    <Conversation key={id ?? 'empty'} csrf={csrf} id={id} session={sessions.rows.find((s) => s.session_id === id)} draft={operations.get(id ?? '')} onDraft={(change) => { if (id) operations.update(id, change) }} onSend={(turn) => { if (id) void operations.send(id, turn) }} onRail={() => setRailOpen(true)} onCollapse={() => setCollapsed((old) => !old)} collapsed={collapsed} onSchedule={setSchedule} onSchedules={setSchedules} />
    {creating && <CreateDialog csrf={csrf} projects={projects} onClose={() => setCreating(false)} onCreated={(sessionId) => { if (sessionId) select(sessionId); void sessions.refresh() }} />}
    {schedule && <ScheduleDialog csrf={csrf} target={schedule} prompt={operations.get(schedule.session_id).text} onClose={() => setSchedule(undefined)} />}
    {schedules && <SchedulesDrawer csrf={csrf} target={schedules} onClose={() => setSchedules(undefined)} />}
  </main>
}
