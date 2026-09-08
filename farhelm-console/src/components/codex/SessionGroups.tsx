import { ClockCircleOutlined, CloseCircleOutlined, FolderOpenOutlined, LoadingOutlined } from '@ant-design/icons'
import { Conversations } from '@ant-design/x'
import { Badge, Button } from 'antd'
import { memo, useMemo, useState } from 'react'
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

export function SessionGroups({ rows, projects, agents, selected, onSelect }: { rows: CodexSession[]; projects: ProjectCandidate[]; agents: AgentSummary[]; selected?: string; onSelect: (id: string) => void }) {
  const [closedProjects, setClosedProjects] = useState<Record<string, boolean>>({})
  const groups = useMemo(() => {
    const names = new Map(projects.map((project) => [JSON.stringify([project.agent_id, project.suggested_project_id]), project.display_name]))
    const devices = new Map(agents.map((agent) => [agent.agent_id, agent]))
    const result = new Map<string, { label: string; device: string; online?: boolean }>()
    for (const row of rows) {
      const key = JSON.stringify([row.agent_id, row.project_id])
      if (result.has(key)) continue
      const agent = devices.get(row.agent_id)
      result.set(key, { label: names.get(key) || row.project_id, device: agent?.hostname || row.agent_id, online: agent?.online })
    }
    return result
  }, [rows, projects, agents])
  return <Conversations className="session-conversations" activeKey={selected} onActiveChange={onSelect}
    groupable={{ collapsible: true,
      expandedKeys: [...groups.keys()].filter((key) => !closedProjects[key]),
      onExpand: (keys) => setClosedProjects((old) => ({ ...old, ...Object.fromEntries([...groups.keys()].map((key) => [key, !keys.includes(key)])) })),
      label: (key) => {
        const { label, device, online } = groups.get(key)!
        const status = online === undefined ? undefined : `设备${online ? '在线' : '离线'}`
        return <Button type="text" className="session-group-heading" aria-label={`项目 ${label} · ${device}`} aria-description={status} aria-expanded={!closedProjects[key]} onClick={(event) => { event.stopPropagation(); setClosedProjects((old) => ({ ...old, [key]: !old[key] })) }}>
          <FolderOpenOutlined aria-hidden /><span className="project-name" title={label}>{label}</span><span className="project-device" title={device}>{device}</span>
          {status && <Badge status={online ? 'success' : 'default'} title={status} aria-hidden />}
        </Button>
      },
    }}
    items={rows.map((session) => ({ key: session.session_id, group: JSON.stringify([session.agent_id, session.project_id]), className: 'session-row', label:
      <SessionRow id={session.session_id} label={sessionName(session)} state={session.state} updated={session.updated_at_unix} active={session.session_id === selected} />,
    }))} />
}
