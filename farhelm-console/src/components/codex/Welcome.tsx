import { ArrowRightOutlined, BarChartOutlined, BugOutlined, FolderOpenOutlined, MessageOutlined, PlusOutlined, UnorderedListOutlined } from '@ant-design/icons'
import { Button } from 'antd'
import type { CodexSession } from '../../api/features'
import { sessionName } from './presentation'

const starters = [
  { icon: <BarChartOutlined />, label: '分析训练结果', prompt: '请检查这个项目的训练结果，说明关键发现和下一步建议。' },
  { icon: <BugOutlined />, label: '检查代码问题', prompt: '请检查这个项目可能存在的问题，按影响程度说明原因和改进建议。' },
  { icon: <UnorderedListOutlined />, label: '规划下一步', prompt: '请梳理这个项目的当前状态，并给出下一步工作的优先顺序。' },
]

export function Welcome({ session, recent, hasDraft, onNew, onBrowse, onSelect, onPrompt }: { session?: CodexSession; recent: CodexSession[]; hasDraft: boolean; onNew: () => void; onBrowse: () => void; onSelect: (id: string) => void; onPrompt: (text: string) => void }) {
  return <div className={`workspace-welcome ${session ? 'session-welcome' : ''}`}>
    <div className="welcome-mark"><img src="/farhelm-mark.svg" width="40" height="40" alt="" /></div>
    <span className="welcome-eyebrow">CODEX 工作区</span>
    <h2>{session ? '准备好，开始下一步' : '继续你的工作'}</h2>
    <p>{session ? <>当前项目 <strong>{session.project_id}</strong>。描述你的目标，Codex 会从这里开始。</> : '从一个会话继续，或在项目中开启新的任务。'}</p>
    {!session ? <>
      <div className="welcome-actions"><Button type="primary" icon={<PlusOutlined />} onClick={onNew} aria-label="开启新会话">新建会话</Button><Button type="text" icon={<FolderOpenOutlined />} onClick={onBrowse}>浏览会话</Button></div>
      {recent.length > 0 && <div className="recent-sessions"><h3>最近的会话</h3>{recent.map((row) => <Button type="text" className="recent-session" aria-label={`打开会话：${sessionName(row)}`} key={row.session_id} onClick={() => onSelect(row.session_id)}><MessageOutlined /><span><strong>{sessionName(row)}</strong><small>{row.agent_id} / {row.project_id}</small></span><ArrowRightOutlined /></Button>)}</div>}
    </> : !hasDraft && <div className="prompt-starters" aria-label="指令建议">{starters.map((starter) => <Button key={starter.label} aria-label={starter.label} icon={starter.icon} onClick={() => onPrompt(starter.prompt)}>{starter.label}<ArrowRightOutlined /></Button>)}</div>}
  </div>
}

export function ActivityIndicator() {
  return <span className="activity-indicator" aria-hidden="true"><i /><i /><i /></span>
}
