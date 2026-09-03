import { CheckCircleOutlined, DisconnectOutlined, ReloadOutlined } from '@ant-design/icons'
import { Alert, Button, Card, Empty, List, Space, Spin, Tag, Typography } from 'antd'

import type { AgentsState } from '../hooks/useAgents'

function lastSeen(unix: number) {
  return new Intl.DateTimeFormat('zh-CN', {
    dateStyle: 'short',
    timeStyle: 'medium',
  }).format(new Date(unix * 1000))
}

export function AgentListPage({ agents, onRefresh }: { agents: AgentsState; onRefresh: () => void }) {
  return (
    <section aria-labelledby="agents-title" className="feature-page">
      <div className="page-heading">
        <div>
          <Typography.Text className="eyebrow">TRAINING HOSTS</Typography.Text>
          <Typography.Title id="agents-title" level={1}>服务器</Typography.Title>
          <Typography.Paragraph type="secondary">这里只显示训练服务器主动上报的真实心跳。</Typography.Paragraph>
        </div>
        <Button icon={<ReloadOutlined />} onClick={onRefresh} loading={agents.state === 'loading'}>刷新状态</Button>
      </div>

      {agents.state === 'error' && <Alert showIcon type="warning" title="无法读取 Agent 列表" description={agents.message} action={<Button onClick={onRefresh}>重试</Button>} />}
      {agents.state === 'loading' && <Card className="agent-list-card"><Space><Spin size="small" /><span>正在读取 Agent 心跳…</span></Space></Card>}
      {agents.state === 'ready' && agents.data.agents.length === 0 && (
        <Card className="agent-list-card"><Empty description="尚未收到 Agent 心跳。请在训练服务器安装并启动 farhelm-agent。" /></Card>
      )}
      {agents.state === 'ready' && agents.data.agents.length > 0 && (
        <Card className="agent-list-card" styles={{ body: { padding: 0 } }}>
          <List
            dataSource={agents.data.agents}
            renderItem={(agent) => (
              <List.Item className="agent-row">
                <List.Item.Meta
                  title={<Space wrap><span>{agent.hostname}</span>{agent.online ? <Tag color="success" icon={<CheckCircleOutlined />}>在线</Tag> : <Tag icon={<DisconnectOutlined />}>离线</Tag>}</Space>}
                  description={<span className="agent-id">{agent.agent_id}</span>}
                />
                <dl className="agent-facts">
                  <div><dt>版本</dt><dd>{agent.agent_version}</dd></div>
                  <div><dt>最后心跳</dt><dd>{lastSeen(agent.last_seen_unix)}</dd></div>
                </dl>
              </List.Item>
            )}
          />
        </Card>
      )}
    </section>
  )
}
