import { CloseOutlined, FilterOutlined, PlusOutlined, SearchOutlined, SettingOutlined } from '@ant-design/icons'
import { Alert, Button, Checkbox, Drawer, Empty, Input, Popover, Segmented, Select, Skeleton } from 'antd'
import { useEffect, useMemo, useState } from 'react'
import { SessionGroups } from './codex/SessionGroups'
import { flushSync } from 'react-dom'
import { useSearchParams } from 'react-router-dom'
import { type ProjectCandidate, type CodexSession } from '../api/features'
import type { AgentSummary } from '../api/agents'
import { Conversation } from './codex/Conversation'
import { CreateDialog, ScheduleDialog, SchedulesDrawer } from './codex/Dialogs'
import { errorText } from './codex/presentation'
import { useOperations } from './codex/useOperations'
import { useSessions, type ArchiveFilter } from './codex/useSessions'
import { useProjects } from '../hooks/useProjects'
import { ProjectsDrawer } from './codex/ProjectsDrawer'
import './codex/workspace.css'
import './codex/markdown.css'

function ScopeFilter({ value, options, onChange }: { value?: string; options: { value: string; label: string }[]; onChange: (value?: string) => void }) {
  const [open, setOpen] = useState(false)
  return <Popover trigger="click" placement="bottomRight" open={open} onOpenChange={setOpen} title="会话范围" content={<div className="session-filter-popover"><label>服务器与项目</label><Select aria-label="筛选项目" showSearch allowClear optionFilterProp="label" placeholder="全部服务器与项目" value={value} onChange={(next) => { onChange(next); setOpen(false) }} options={options} /></div>}><Button type="text" className={value ? 'scope-active' : undefined} icon={<FilterOutlined />} aria-label="筛选服务器与项目" aria-expanded={open} /></Popover>
}

export function CodexPage({ csrf, agents }: { csrf: string; agents: AgentSummary[] }) {
  const [params, setParams] = useSearchParams(); const id = params.get('session') ?? undefined
  const [query, setQuery] = useState(''); const [archive, setArchive] = useState<ArchiveFilter>('false')
  const [scope, setScope] = useState<string>(); const [railOpen, setRailOpen] = useState(false); const [collapsed, setCollapsed] = useState(false)
  const model = useProjects(csrf); const projects = model.projects; const projectQuery = model.catalog; const projectError = projectQuery.error || model.preferences.error ? errorText(projectQuery.error ?? model.preferences.error) : undefined
  const [managing, setManaging] = useState(false); const [manageAfterRail, setManageAfterRail] = useState(false); const [initialProject, setInitialProject] = useState<ProjectCandidate>(); const [includeHidden, setIncludeHidden] = useState(false)
  const [creating, setCreating] = useState(false); const [schedule, setSchedule] = useState<CodexSession>(); const [schedules, setSchedules] = useState<CodexSession>()
  const [createAfterRail, setCreateAfterRail] = useState(false)
  const scopes = useMemo(() => {
    const agents = [...new Set(projects.map((p) => p.agent_id))]
    return agents.flatMap((agent_id) => [{ value: JSON.stringify([agent_id]), label: `${agent_id} / 全部项目`, agent_id, project_id: undefined as string | undefined }, ...projects.filter((p) => p.agent_id === agent_id && p.state === 'approved').map((p) => ({ value: JSON.stringify([agent_id, p.suggested_project_id]), label: `${agent_id} / ${p.display_name}`, agent_id, project_id: p.suggested_project_id }))])
  }, [projects])
  const scoped = scopes.find((option) => option.value === scope)
  const sessions = useSessions(csrf, query, archive, scoped?.agent_id, scoped?.project_id, !includeHidden, model.preferences.data?.revision)
  const visibleProjects = (includeHidden ? model.approved : model.visible).filter((p) => (!scoped?.agent_id || p.agent_id === scoped.agent_id) && (!scoped?.project_id || p.suggested_project_id === scoped.project_id))
  const operations = useOperations(csrf)
  const [entering, setEntering] = useState(true)
  useEffect(() => {
    // Only the first workspace entry moves the composer; cached session switches stay immediate.
    const timer = window.setTimeout(() => setEntering(false), 280)
    return () => window.clearTimeout(timer)
  }, [])
  useEffect(() => {
    const viewport = window.visualViewport
    const resize = () => { document.documentElement.style.setProperty('--codex-height', `${viewport?.height ?? innerHeight}px`); document.documentElement.style.setProperty('--codex-top', `${viewport?.offsetTop ?? 0}px`) }
    resize(); viewport?.addEventListener('resize', resize); viewport?.addEventListener('scroll', resize); window.addEventListener('resize', resize)
    return () => { viewport?.removeEventListener('resize', resize); viewport?.removeEventListener('scroll', resize); window.removeEventListener('resize', resize); document.documentElement.style.removeProperty('--codex-height'); document.documentElement.style.removeProperty('--codex-top') }
  }, [])
  useEffect(() => {
    const search = (event: KeyboardEvent) => {
      if (!(event.ctrlKey || event.metaKey) || event.key.toLowerCase() !== 'k' || event.isComposing || innerWidth < 768) return
      event.preventDefault(); setCollapsed(false)
      requestAnimationFrame(() => document.querySelector<HTMLInputElement>('.codex-desktop-rail input[aria-label="搜索全部会话"]')?.focus())
    }
    window.addEventListener('keydown', search)
    return () => window.removeEventListener('keydown', search)
  }, [])
  const select = (sessionId?: string) => { const next = new URLSearchParams(params); if (sessionId) next.set('session', sessionId); else next.delete('session'); flushSync(() => setParams(next)); setRailOpen(false) }
  const newSession = (project?: ProjectCandidate) => {
    setInitialProject(project)
    // Let the Drawer restore focus before mounting a Modal with its own focus trap.
    if (railOpen) { setCreateAfterRail(true); setRailOpen(false) }
    else setCreating(true)
  }
  const manage = () => { if (railOpen) { setManageAfterRail(true); setRailOpen(false) } else setManaging(true) }
  const browse = () => {
    if (innerWidth < 768) setRailOpen(true)
    else { setCollapsed(false); requestAnimationFrame(() => document.querySelector<HTMLInputElement>('.codex-desktop-rail input[aria-label="搜索全部会话"]')?.focus()) }
  }
  const rail = <aside className="codex-rail" aria-label="项目和会话">
    <div className="codex-rail-head"><h2>会话</h2><div className="rail-heading-actions"><Button type="text" icon={<SettingOutlined />} onClick={manage} aria-label="项目管理" />{projects.length > 0 && <ScopeFilter value={scope} options={scopes} onChange={setScope} />}<Button type="text" className="new-session-button" icon={<PlusOutlined />} onClick={() => newSession()} aria-label="新建会话">新建</Button></div></div>
    <Input className="session-search" allowClear prefix={<SearchOutlined />} suffix={<kbd className="desktop-only">{navigator.platform.includes('Mac') ? '⌘ K' : 'Ctrl K'}</kbd>} placeholder="搜索全部会话" aria-label="搜索全部会话" value={query} onChange={(e) => { setQuery(e.target.value); if (!e.target.value) setIncludeHidden(false) }} />
    <div className="session-filter-row"><Segmented block className="archive-tabs" value={archive} onChange={(value) => { setArchive(value as ArchiveFilter) }} options={[{ label: '当前', value: 'false' }, { label: '归档', value: 'true' }, { label: '全部', value: 'all' }]} /></div>
    {query && <Checkbox checked={includeHidden} onChange={(e) => setIncludeHidden(e.target.checked)}>包含隐藏项目</Checkbox>}
    {!model.visible.length && model.approved.length > 0 && <Alert type="info" title="全部项目已隐藏" action={<Button onClick={manage}>恢复项目显示</Button>} />}
    {!model.approved.length && <Button icon={<PlusOutlined />} onClick={manage}>添加项目</Button>}
    {scoped && <div className="active-session-scope"><span title={scoped.label}>{scoped.label}</span><Button type="text" icon={<CloseOutlined />} aria-label="清除项目筛选" onClick={() => setScope(undefined)} /></div>}
    {(sessions.error || projectError) && <Alert type="warning" title={sessions.error ?? projectError} action={<Button type="text" onClick={() => { void sessions.refresh(); if (projectError) void projectQuery.refetch() }}>重试</Button>} />}
    {sessions.incomplete.length > 0 && <div className="session-partial" role="status">部分会话信息不可用：{sessions.incomplete.map((a) => `${a.agent_id}（${a.reason === 'agent_offline' ? '离线' : a.reason === 'agent_upgrade_required' ? '需要升级 Agent' : '读取失败'}）`).join('、')}<Button type="link" onClick={() => void sessions.refresh()}>重试</Button></div>}
    <div className="codex-session-groups" onKeyDown={(event) => {
      if (['Enter', ' '].includes(event.key) && (event.target as HTMLElement).classList.contains('session-select')) { event.preventDefault(); (event.target as HTMLElement).click(); return }
      if (!['ArrowUp', 'ArrowDown', 'Home', 'End'].includes(event.key)) return
      const buttons = [...event.currentTarget.querySelectorAll<HTMLButtonElement>('.session-select')].filter((button) => button.offsetParent !== null); const index = buttons.indexOf(document.activeElement as HTMLButtonElement); if (index < 0) return
      event.preventDefault(); const next = event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1 : Math.max(0, Math.min(buttons.length - 1, index + (event.key === 'ArrowDown' ? 1 : -1))); buttons[next]?.focus()
    }}>{sessions.loading && !sessions.rows.length ? <Skeleton active paragraph={{ rows: 6 }} /> : visibleProjects.length ? <SessionGroups rows={sessions.rows} projects={visibleProjects} agents={agents} selected={id} onSelect={select} onNew={newSession} onManage={manage} onProject={(p) => setScope(JSON.stringify([p.agent_id, p.suggested_project_id]))} /> : !sessions.error && <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={query ? '当前范围内没有匹配的会话' : '当前范围内没有会话'} />}{sessions.cursor && <Button block loading={sessions.loading} onClick={() => void sessions.more()}>加载更多会话</Button>}</div>
  </aside>
  return <div className={`codex-workspace ${collapsed ? 'rail-collapsed' : ''} ${entering ? 'workspace-entering' : ''}`}><div className="codex-desktop-rail">{rail}</div><Drawer className="codex-mobile-drawer" aria-label="项目和会话" placement="left" open={railOpen} onClose={() => setRailOpen(false)} afterOpenChange={(open) => { if (!open && manageAfterRail) { setManageAfterRail(false); setManaging(true) } if (!open && createAfterRail) { setCreateAfterRail(false); setCreating(true) } }} size={Math.min(320, window.innerWidth - 24)} closable={false}>{rail}</Drawer>
    <Conversation recent={sessions.rows.slice(0, 3)} onSelect={select} onNew={newSession} onBrowse={browse} key={id ?? 'empty'} csrf={csrf} id={id} session={sessions.rows.find((s) => s.session_id === id)} draft={operations.get(id ?? '')} onDraft={(change) => { if (id) operations.update(id, change) }} onSend={(turn) => { if (id) void operations.send(id, turn) }} onRail={() => setRailOpen(true)} onCollapse={() => setCollapsed((old) => !old)} collapsed={collapsed} onSchedule={setSchedule} onSchedules={setSchedules} />
    {creating && <CreateDialog csrf={csrf} projects={projects} initialProject={initialProject} onClose={() => setCreating(false)} onCreated={(sessionId) => { if (sessionId) select(sessionId); void sessions.refresh() }} />}
    <ProjectsDrawer csrf={csrf} agents={agents} open={managing} onClose={() => setManaging(false)} onNew={newSession} />
    {schedule && <ScheduleDialog csrf={csrf} target={schedule} prompt={operations.get(schedule.session_id).text} onClose={() => setSchedule(undefined)} />}
    {schedules && <SchedulesDrawer csrf={csrf} target={schedules} onClose={() => setSchedules(undefined)} />}
  </div>
}
