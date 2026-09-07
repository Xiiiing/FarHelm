import { ClockCircleOutlined, CloseCircleOutlined, DesktopOutlined, FolderOpenOutlined, LoadingOutlined, RightOutlined } from '@ant-design/icons'
import { Conversations } from '@ant-design/x'
import { Button, Collapse } from 'antd'
import { memo, useMemo, useState } from 'react'
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

export function SessionGroups({ rows, projects, selected, onSelect }: { rows: CodexSession[]; projects: ProjectCandidate[]; selected?: string; onSelect: (id: string) => void }) {
  const [closedServers, setClosedServers] = useState<Record<string, boolean>>({})
  const [closedProjects, setClosedProjects] = useState<Record<string, boolean>>({})
  const servers = useMemo(() => {
    const result = new Map<string, Map<string, CodexSession[]>>()
    for (const row of rows) {
      const server = result.get(row.agent_id) ?? new Map<string, CodexSession[]>()
      const project = server.get(row.project_id) ?? []
      project.push(row); server.set(row.project_id, project); result.set(row.agent_id, server)
    }
    return [...result].map(([id, groups]) => ({ id, groups: [...groups], rows: [...groups.values()].flat() }))
  }, [rows])
  return <Collapse ghost className="server-groups" activeKey={servers.filter((server) => !closedServers[server.id]).map((server) => server.id)}
    onChange={(keys) => setClosedServers(Object.fromEntries(servers.map((server) => [server.id, !keys.includes(server.id)])))}
    expandIcon={({ isActive }) => <RightOutlined className="disclosure-icon" rotate={isActive ? 90 : 0} />}
    items={servers.map((server) => ({ key: server.id, className: 'server-group', label: <span className="server-heading"><DesktopOutlined /><strong>{server.id}</strong><span className="server-count">{server.rows.length}</span></span>, children:
      <Conversations className="session-conversations" activeKey={selected} onActiveChange={onSelect}
        groupable={{ collapsible: true,
          expandedKeys: server.groups.filter(([group]) => !closedProjects[JSON.stringify([server.id, group])]).map(([group]) => group),
          onExpand: (keys) => setClosedProjects((old) => ({ ...old, ...Object.fromEntries(server.groups.map(([group]) => [JSON.stringify([server.id, group]), !keys.includes(group)])) })),
          label: (group) => {
            const key = JSON.stringify([server.id, group])
            const label = projects.find((project) => project.agent_id === server.id && project.suggested_project_id === group)?.display_name ?? group
            return <Button type="text" className="session-group-heading" aria-label={`项目 ${label}`} aria-expanded={!closedProjects[key]} onClick={(event) => { event.stopPropagation(); setClosedProjects((old) => ({ ...old, [key]: !old[key] })) }}><FolderOpenOutlined /><span title={label}>{label}</span></Button>
          },
        }}
        items={server.rows.map((session) => ({ key: session.session_id, group: session.project_id, className: 'session-row', label:
          <SessionRow id={session.session_id} label={sessionName(session)} state={session.state} updated={session.updated_at_unix} active={session.session_id === selected} />,
        }))} />,
    }))} />
}
