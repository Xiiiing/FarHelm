import {
  ArrowRightOutlined, BellOutlined, CheckCircleOutlined, CodeOutlined,
  DesktopOutlined, DisconnectOutlined, ExclamationCircleOutlined,
  ExperimentOutlined, ReloadOutlined,
} from '@ant-design/icons'
import { useQuery } from '@tanstack/react-query'
import { Alert, Button, Skeleton, Spin } from 'antd'
import { Link } from 'react-router-dom'

import type { AgentSummary } from '../api/agents'
import { json } from '../api/features'
import type { HealthResponse } from '../api/health'
import type { AgentsState } from '../hooks/useAgents'
import './overview.css'

type HubHealth =
  | { state: 'checking'; data?: undefined; message?: undefined }
  | { state: 'online'; data: HealthResponse; message?: undefined }
  | { state: 'offline'; data?: undefined; message: string }

type OverviewCounts = {
  active_experiments: number
  active_sessions: number
  unread_notifications: number
  failed_tasks: number
}

const metrics = [
  { key: 'active_experiments', label: '活动实验', detail: '查看实验进展', to: '/experiments', icon: <ExperimentOutlined /> },
  { key: 'active_sessions', label: '活动会话', detail: '进入 Codex 工作区', to: '/codex', icon: <CodeOutlined /> },
  { key: 'unread_notifications', label: '未读通知', detail: '查看最新结果', to: '/notifications', icon: <BellOutlined /> },
  { key: 'failed_tasks', label: '失败 / 未知结果', detail: '包含历史结果记录', to: '/notifications', icon: <ExclamationCircleOutlined /> },
] as const

async function fetchOverview(signal: AbortSignal): Promise<OverviewCounts> {
  const value = await json<OverviewCounts>('/api/v1/overview', signal)
  if (!metrics.every(({ key }) => Number.isSafeInteger(value?.[key]) && value[key] >= 0)) throw new Error('Hub 返回的业务统计无效')
  return value
}

const heartbeatFormat = new Intl.DateTimeFormat('zh-CN', { hour: '2-digit', minute: '2-digit', second: '2-digit' })

function codexStatus(agent: AgentSummary) {
  if (agent.credential_state === 'needs_pairing') return '需要重新配对'
  if (!agent.codex) return 'Codex 状态未上报'
  const labels = { ready: 'Codex 已就绪', starting: 'Codex 初始化中', unconfigured: 'Codex 未配置', unavailable: 'Codex 暂不可用', login_required: 'Codex 需要本机登录' }
  return labels[agent.codex.state]
}

export function Overview({ health, agents, onRefresh }: { health: HubHealth; agents: AgentsState; onRefresh: () => void }) {
  const statistics = useQuery({ queryKey: ['overview'], queryFn: ({ signal }) => fetchOverview(signal), staleTime: 15_000, refetchInterval: 15_000 })
  const onlineAgents = agents.state === 'ready' ? agents.data.agents.filter((agent) => agent.online) : []
  const offlineCount = agents.state === 'ready' ? agents.data.agents.length - onlineAgents.length : 0
  const refresh = () => { onRefresh(); void statistics.refetch() }

  return (
    <section aria-labelledby="overview-title" className="feature-page overview-page">
      <header className="overview-heading">
        <div>
          <span className="overview-kicker">工作空间</span>
          <h1 id="overview-title">运行总览</h1>
          <p>查看正在进行的工作，从上次停下的地方继续。</p>
        </div>
        <Button aria-label="刷新状态" icon={<ReloadOutlined />} onClick={refresh} loading={health.state === 'checking' || statistics.isFetching}>刷新状态</Button>
      </header>

      <div className="overview-connection" aria-live="polite">
        {health.state === 'checking' && <><Spin size="small" /><span>正在验证 Hub 连接…</span></>}
        {health.state === 'online' && <><CheckCircleOutlined className="overview-ready" /><span>在线 · 已验证</span><span className="overview-connection-note">Hub 连接正常</span></>}
        {health.state === 'offline' && <><DisconnectOutlined /><span>离线 · 无实时数据</span></>}
      </div>
      {health.state === 'offline' && <Alert showIcon type="warning" title="Hub 当前不可用" description={health.message} action={<Button onClick={refresh}>重试</Button>} />}
      {statistics.isError && <Alert showIcon type="warning" title={statistics.data ? '统计刷新失败，当前显示上次结果' : '业务统计暂时不可用'} description={`统计读取失败：${statistics.error.message}`} action={<Button onClick={() => void statistics.refetch()}>重新读取统计</Button>} />}

      <nav className="overview-metrics" aria-label="工作动态">
        {metrics.map(({ key, label, detail, to, icon }) => <Link className={`overview-metric${key === 'failed_tasks' && statistics.data?.[key] ? ' has-attention' : ''}`} to={to} key={key}>
          <span className="overview-metric-label">{icon}{label}</span>
          <span className="overview-metric-value">{statistics.data ? statistics.data[key] : statistics.isPending ? <Skeleton.Input active size="small" /> : <span aria-label="数量不可用">—</span>}</span>
          <span className="overview-metric-detail">{detail}<ArrowRightOutlined /></span>
        </Link>)}
      </nav>

      <div className="overview-workspace">
        <section className="overview-servers" aria-labelledby="overview-servers-title">
          <header className="overview-section-heading">
            <div><h2 id="overview-servers-title">在线服务器</h2><span>{agents.state === 'ready' ? `${onlineAgents.length} 台在线${offlineCount ? ` · ${offlineCount} 台离线` : ''}` : agents.state === 'error' ? '状态暂时不可用' : '正在核对连接状态'}</span></div>
            <Link to="/agents" className="overview-text-link">管理服务器<ArrowRightOutlined /></Link>
          </header>
          {agents.state === 'loading' && <div className="overview-server-placeholder"><Skeleton active paragraph={{ rows: 2 }} title={{ width: '40%' }} /></div>}
          {agents.state === 'error' && <div className="overview-server-placeholder"><ExclamationCircleOutlined /><h3>暂时无法读取服务器</h3><p>服务器读取失败：{agents.message}</p><Button onClick={onRefresh}>重新读取服务器</Button></div>}
          {agents.state === 'ready' && onlineAgents.length === 0 && <div className="overview-server-placeholder"><DesktopOutlined /><h3>{offlineCount ? '当前没有在线服务器' : '连接你的第一台服务器'}</h3><p>{offlineCount ? '已有服务器暂时离线，恢复连接后会出现在这里。' : '连接 Agent 后，在这里查看服务器和 Codex 就绪状态。'}</p><Link to="/agents" className="overview-text-link">{offlineCount ? '查看服务器状态' : '前往添加服务器'}<ArrowRightOutlined /></Link></div>}
          {onlineAgents.length > 0 && <ul className="overview-server-list">{onlineAgents.slice(0, 6).map((agent) => <li key={agent.agent_id}>
            <span className="overview-server-icon"><DesktopOutlined /></span>
            <div className="overview-server-name"><strong>{agent.hostname}</strong><span title={new Date(agent.last_seen_unix * 1000).toLocaleString('zh-CN')}>最近心跳 {heartbeatFormat.format(new Date(agent.last_seen_unix * 1000))}</span></div>
            <div className="overview-server-status"><span><CheckCircleOutlined className="overview-ready" />在线</span><span className={agent.codex?.state === 'ready' && agent.credential_state !== 'needs_pairing' ? '' : 'overview-server-unready'}>{codexStatus(agent)}</span></div>
          </li>)}</ul>}
          {onlineAgents.length > 6 && <Link className="overview-server-more overview-text-link" to="/agents">查看全部 {onlineAgents.length} 台在线服务器<ArrowRightOutlined /></Link>}
        </section>

        <aside className="overview-continue" aria-labelledby="overview-continue-title">
          <h2 id="overview-continue-title">继续工作</h2>
          <Link className="overview-work-link" to="/codex"><CodeOutlined /><span><strong>打开 Codex</strong><small>继续会话，查看回复与执行进展</small></span><ArrowRightOutlined /></Link>
          <Link className="overview-work-link" to="/experiments"><ExperimentOutlined /><span><strong>查看实验结果</strong><small>查看脚本上报与进程监控结果</small></span><ArrowRightOutlined /></Link>
          <Link className="overview-work-link" to="/notifications"><BellOutlined /><span><strong>打开通知中心</strong><small>回顾完成、失败和未知结果</small></span><ArrowRightOutlined /></Link>
          {health.state === 'online' && <details className="overview-hub-details"><summary>Hub 连接详情</summary><dl><div><dt>服务</dt><dd>{health.data.service}</dd></div><div><dt>版本</dt><dd>{health.data.version}</dd></div><div><dt>协议</dt><dd>{health.data.protocol}</dd></div></dl></details>}
        </aside>
      </div>
    </section>
  )
}
