import { ClockCircleOutlined, CloseCircleOutlined, FolderOpenOutlined, LoadingOutlined, PlusOutlined, EllipsisOutlined } from '@ant-design/icons'
import { Conversations } from '@ant-design/x'
import { Badge, Button, Dropdown } from 'antd'
import { memo, useState } from 'react'
import type { AgentSummary } from '../../api/agents'
import type { CodexSession, ProjectCandidate } from '../../api/features'
import { sessionName, stateNames } from './presentation'

const sessionDay = new Intl.DateTimeFormat('zh-CN', { month: 'numeric', day: 'numeric' })
const sessionTimestamp = new Intl.DateTimeFormat('zh-CN', { year: 'numeric', month: 'numeric', day: 'numeric', hour: 'numeric', minute: 'numeric', second: 'numeric', hour12: false })

// Draft keystrokes and another session's selection must not re-render every row.
const SessionRow = memo(function SessionRow({ id, label, state, updated, active }: { id: string; label: string; state: string; updated: number; active: boolean }) {
  return <Button type="text" className="session-select" block aria-label={label} aria-current={active ? 'page' : undefined} aria-pressed={active} data-session-id={id}>
    <span className="session-copy"><span title={label}>{label}</span>
      {['running', 'queued', 'creating', 'failed', 'orphaned'].includes(state) && <small className={['failed', 'orphaned'].includes(state) ? 'session-warning' : ''}>{state === 'running' ? <LoadingOutlined /> : ['failed', 'orphaned'].includes(state) ? <CloseCircleOutlined /> : <ClockCircleOutlined />} {stateNames[state]}</small>}
    </span>
    <time className="session-time" dateTime={new Date(updated * 1000).toISOString()} title={sessionTimestamp.format(updated * 1000)}>{sessionDay.format(updated * 1000)}</time>
  </Button>
})

export function SessionGroups({ rows, projects, agents, selected, onSelect, onNew, onManage, onProject }: { rows: CodexSession[]; projects: ProjectCandidate[]; agents: AgentSummary[]; selected?: string; onSelect: (id: string) => void; onNew?: (project: ProjectCandidate) => void; onManage?: () => void; onProject?: (project: ProjectCandidate) => void }) {
  const [closedProjects, setClosedProjects] = useState<Record<string, boolean>>({})
  const [closedSections, setClosedSections] = useState<Record<string, boolean>>({})
  return <div className="session-conversations">{projects.filter((p) => p.state === 'approved').map((project) => {
    const key = JSON.stringify([project.agent_id, project.suggested_project_id])
    const agent = agents.find((a) => a.agent_id === project.agent_id)
    const device = agent?.hostname ?? project.agent_id
    const sessions = rows.filter((s) => s.agent_id === project.agent_id && s.project_id === project.suggested_project_id)
    return <section key={key} aria-label={`项目组 ${project.display_name} · ${device}`}>
      <div className="project-group-header"><Button type="text" className="session-group-heading" aria-label={`项目 ${project.display_name} · ${device}`} aria-description={agent ? `设备${agent.online ? '在线' : '离线'}` : undefined} aria-expanded={!closedProjects[key]} onClick={() => setClosedProjects((old) => ({ ...old, [key]: !old[key] }))}>
        <FolderOpenOutlined aria-hidden /><span className="project-name" title={project.display_name}>{project.display_name}</span><span className="project-device" title={device}>{device}</span>{agent && <Badge status={agent.online ? 'success' : 'default'} title={`设备${agent.online ? '在线' : '离线'}`} aria-hidden />}
      </Button>{onNew && <Dropdown trigger={['click']} menu={{ items: [{ key: 'new', icon: <PlusOutlined />, label: '新建会话', disabled: !agent?.online }, { key: 'sessions', label: '查看项目全部会话' }, { key: 'manage', label: '展示、隐藏与项目设置' }], onClick: ({ key }) => { if (key === 'new') onNew(project); if (key === 'sessions') onProject?.(project); if (key === 'manage') onManage?.() } }}><Button type="text" icon={<EllipsisOutlined />} aria-label={`管理项目 ${project.display_name} · ${device}`} /></Dropdown>}</div>
      {!closedProjects[key] && (sessions.length ? [...new Set(sessions.map((session) => session.section_name ?? '未分组'))].sort((a, b) => a.localeCompare(b)).map((section) => { const sectionKey = `${key}:${section}`; const members = sessions.filter((session) => (session.section_name ?? '未分组') === section).sort((a, b) => Number(b.is_pinned === true) - Number(a.is_pinned === true)); return <div key={sectionKey} className="native-session-section"><Button type="text" className="native-section-heading" aria-expanded={!closedSections[sectionKey]} onClick={() => setClosedSections((old) => ({ ...old, [sectionKey]: !old[sectionKey] }))}>{section} · {members.length}</Button>{!closedSections[sectionKey] && <Conversations className="session-conversations" activeKey={selected} onActiveChange={onSelect} items={members.map((session) => ({ key: session.session_id, className: 'session-row', label: <SessionRow id={session.session_id} label={sessionName(session)} state={session.state} updated={session.updated_at_unix} active={session.session_id === selected} /> }))} />}</div> }) : <div className="project-empty-state">{!agent?.online ? '服务器离线' : project.sync_state === 'failed' ? '已接入，历史同步失败' : project.sync_state === 'pending' ? '历史同步中' : project.session_count ? '此页暂无会话，可查看项目全部会话' : '尚无会话'}{onNew && <Button type="link" aria-label={`为项目 ${project.display_name} 创建会话 · ${device}`} disabled={!agent?.online} onClick={() => onNew(project)}>新建会话</Button>}{project.session_count > 0 && onProject && <Button type="link" onClick={() => onProject(project)}>查看会话</Button>}</div>)}
    </section>
  })}</div>
}
