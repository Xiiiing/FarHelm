import { CheckCircleOutlined, DisconnectOutlined, ReloadOutlined } from '@ant-design/icons'
import { Alert, Button, Card, Col, Row, Space, Spin, Tag, Typography } from 'antd'

import type { HealthResponse } from '../api/health'
import type { AgentsState } from '../hooks/useAgents'

type HubHealth =
  | { state: 'checking'; data?: undefined; message?: undefined }
  | { state: 'online'; data: HealthResponse; message?: undefined }
  | { state: 'offline'; data?: undefined; message: string }

export function Overview({ health, agents, onRefresh }: { health: HubHealth; agents: AgentsState; onRefresh: () => void }) {
  const onlineAgents = agents.state === 'ready' ? agents.data.agents.filter((agent) => agent.online).length : undefined
  return (
    <section aria-labelledby="overview-title" className="feature-page">
      <div className="page-heading">
        <div>
          <Typography.Text className="eyebrow">CONTROL PLANE</Typography.Text>
          <Typography.Title id="overview-title" level={1}>
            运行总览
          </Typography.Title>
          <Typography.Paragraph type="secondary">
            从一个清晰、可验证的入口开始管理远程训练。
          </Typography.Paragraph>
        </div>
        <Button icon={<ReloadOutlined />} onClick={onRefresh} loading={health.state === 'checking'}>
          刷新状态
        </Button>
      </div>

      {health.state === 'offline' && (
        <Alert
          showIcon
          type="warning"
          title="Hub 当前不可用"
          description={health.message}
          action={<Button onClick={onRefresh}>重试</Button>}
        />
      )}

      <Row gutter={[16, 16]} className="status-grid">
        <Col xs={24} lg={12}>
          <Card title="Hub 连接" className="status-card">
            {health.state === 'checking' && (
              <Space><Spin size="small" /><span>正在验证协议与服务状态…</span></Space>
            )}
            {health.state === 'online' && (
              <Space direction="vertical" size={12}>
                <Tag color="success" icon={<CheckCircleOutlined />}>在线 · 已验证</Tag>
                <dl className="facts">
                  <div><dt>服务</dt><dd>{health.data.service}</dd></div>
                  <div><dt>版本</dt><dd>{health.data.version}</dd></div>
                  <div><dt>协议</dt><dd>{health.data.protocol}</dd></div>
                </dl>
              </Space>
            )}
            {health.state === 'offline' && (
              <Tag icon={<DisconnectOutlined />}>离线 · 无实时数据</Tag>
            )}
          </Card>
        </Col>
        <Col xs={24} lg={12}>
          <Card title="系统范围" className="status-card">
            <Typography.Paragraph type="secondary">
              Agent 在线状态来自真实心跳；训练任务与 Codex 会话仍未接入。
            </Typography.Paragraph>
            <Space wrap>
              {onlineAgents === undefined && <Tag>Agent 状态不可用</Tag>}
              {onlineAgents === 0 && <Tag>尚无 Agent 心跳</Tag>}
              {onlineAgents !== undefined && onlineAgents > 0 && <Tag color="success">{onlineAgents} 台 Agent 在线</Tag>}
              <Tag>任务未接入</Tag>
              <Tag>Codex 未连接</Tag>
            </Space>
          </Card>
        </Col>
      </Row>
    </section>
  )
}
