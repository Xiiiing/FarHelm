import { CheckCircleOutlined, DisconnectOutlined, FolderOpenOutlined, PlusOutlined, ReloadOutlined } from '@ant-design/icons'
import { Alert, Button, Card, Empty, Form, Input, List, Modal, Space, Spin, Tag, Typography } from 'antd'
import { useCallback, useEffect, useMemo, useState } from 'react'

import { createPairingCode, type PairingCode } from '../api/agents'
import { fetchProjects, importProjects, type ProjectCandidate } from '../api/features'
import type { AgentsState } from '../hooks/useAgents'

function lastSeen(unix: number) {
  return new Intl.DateTimeFormat('zh-CN', { dateStyle: 'short', timeStyle: 'medium' }).format(new Date(unix * 1000))
}

export function AgentListPage({ csrf, agents, onRefresh }: { csrf: string; agents: AgentsState; onRefresh: () => void }) {
  const [pairingOpen, setPairingOpen] = useState(false)
  const [pairing, setPairing] = useState<PairingCode>()
  const [pairingError, setPairingError] = useState<string>()
  const [projects, setProjects] = useState<ProjectCandidate[]>([])
  const [projectsError, setProjectsError] = useState<string>()
  const [importing, setImporting] = useState(false)
  const [form] = Form.useForm()
  const loadProjects = useCallback(() => void fetchProjects().then(setProjects).catch((reason: unknown) => setProjectsError(reason instanceof Error ? reason.message : '无法读取项目候选')), [])
  useEffect(() => {
    loadProjects()
    if (typeof EventSource === 'undefined') return
    const stream = new EventSource('/api/v1/events/stream'); stream.addEventListener('project.discovered', loadProjects); stream.addEventListener('project.updated', loadProjects)
    return () => stream.close()
  }, [loadProjects])
  const pending = useMemo(() => projects.filter((project) => project.state === 'discovered'), [projects])
  const importAll = async () => {
    setImporting(true); setProjectsError(undefined)
    try {
      const grouped = pending.reduce((result, project) => {
        const items = result.get(project.agent_id) ?? []; items.push(project); result.set(project.agent_id, items); return result
      }, new Map<string, ProjectCandidate[]>())
      for (const [agentId, items] of grouped) await importProjects(csrf, agentId, items.map((item) => item.candidate_id))
      loadProjects()
    } catch (reason) { setProjectsError(reason instanceof Error ? reason.message : '导入失败') }
    finally { setImporting(false) }
  }
  const createCode = async ({ agentId }: { agentId: string }) => {
    setPairingError(undefined)
    try { setPairing(await createPairingCode(csrf, agentId.trim())) }
    catch (reason) { setPairingError(reason instanceof Error ? reason.message : '无法创建配对码') }
  }
  return <section aria-labelledby="agents-title" className="feature-page">
    <div className="page-heading"><div><Typography.Text className="eyebrow">TRAINING HOSTS</Typography.Text><Typography.Title id="agents-title" level={1}>服务器</Typography.Title><Typography.Paragraph type="secondary">添加服务器后只需输入一次 8 位配对码，独立凭据会自动保存。</Typography.Paragraph></div><Space wrap><Button icon={<ReloadOutlined />} onClick={() => { onRefresh(); loadProjects() }} loading={agents.state === 'loading'}>刷新状态</Button><Button type="primary" icon={<PlusOutlined />} onClick={() => { setPairing(undefined); setPairingOpen(true) }}>添加服务器</Button></Space></div>
    {agents.state === 'error' && <Alert showIcon type="warning" title="无法读取 Agent 列表" description={agents.message} action={<Button onClick={onRefresh}>重试</Button>} />}
    {agents.state === 'loading' && <Card className="agent-list-card"><Space><Spin size="small" /><span>正在读取 Agent 心跳…</span></Space></Card>}
    {agents.state === 'ready' && agents.data.agents.length === 0 && <Card className="agent-list-card"><Empty description="尚无服务器。点击“添加服务器”开始配对。" /></Card>}
    {agents.state === 'ready' && agents.data.agents.length > 0 && <Card className="agent-list-card" styles={{ body: { padding: 0 } }}><List dataSource={agents.data.agents} renderItem={(agent) => <List.Item className="agent-row"><List.Item.Meta title={<Space wrap><span>{agent.hostname}</span>{agent.online ? <Tag color="success" icon={<CheckCircleOutlined />}>在线</Tag> : <Tag icon={<DisconnectOutlined />}>离线</Tag>}{agent.credential_state !== 'paired' && <Tag color="warning">需要配对</Tag>}</Space>} description={<span className="agent-id">{agent.agent_id}</span>} /><dl className="agent-facts"><div><dt>版本</dt><dd>{agent.agent_version}</dd></div><div><dt>最后心跳</dt><dd>{lastSeen(agent.last_seen_unix)}</dd></div></dl></List.Item>} /></Card>}

    <div className="page-heading section-heading"><div><Typography.Text className="eyebrow">DISCOVERED PROJECTS</Typography.Text><Typography.Title level={2}>Codex 项目</Typography.Title><Typography.Paragraph type="secondary">Agent 自动识别 Codex 历史会话所在目录；路径只保留在服务器本地。</Typography.Paragraph></div>{pending.length > 0 && <Button type="primary" icon={<FolderOpenOutlined />} loading={importing} onClick={() => void importAll()}>一键导入全部（{pending.length}）</Button>}</div>
    {projectsError && <Alert showIcon closable onClose={() => setProjectsError(undefined)} type="warning" title="项目同步失败" description={projectsError} />}
    <Card className="agent-list-card" styles={{ body: { padding: projects.length ? 0 : 24 } }}>{projects.length === 0 ? <Empty description="等待 Agent 扫描 Codex 历史项目，通常不超过 60 秒。" /> : <List dataSource={projects} renderItem={(project) => <List.Item className="agent-row"><List.Item.Meta title={<Space wrap><span>{project.display_name}</span><Tag color={project.state === 'approved' ? 'success' : 'processing'}>{project.state === 'approved' ? '已导入' : '待确认'}</Tag></Space>} description={`${project.agent_id} · ${project.suggested_project_id}`} /><Typography.Text type="secondary">{project.session_count} 个会话</Typography.Text></List.Item>} />}</Card>

    <Modal title="添加服务器" open={pairingOpen} onCancel={() => setPairingOpen(false)} footer={null} destroyOnHidden>
      {pairingError && <Alert showIcon type="error" title="无法创建配对码" description={pairingError} />}
      {!pairing ? <Form form={form} layout="vertical" onFinish={(values: { agentId: string }) => void createCode(values)}><Form.Item label="Agent 名称" name="agentId" extra="例如 titan、a6000 或 work832" rules={[{ required: true }, { pattern: /^[A-Za-z0-9_.-]{1,64}$/, message: '只可使用字母、数字、点、横线和下划线' }]}><Input autoFocus /></Form.Item><Button type="primary" htmlType="submit" block>生成配对码</Button></Form> : <Space direction="vertical" size="large" style={{ width: '100%' }}><Alert showIcon type="info" title={`在 ${pairing.agent_id} 上运行`} description={<><Typography.Paragraph><Typography.Text code>farhelm-agent pair</Typography.Text></Typography.Paragraph><Typography.Text>按提示输入当前 Hub 地址和下面的 8 位配对码。新安装可直接运行 <Typography.Text code>farhelm-agent install</Typography.Text>。</Typography.Text></>} /><div className="pairing-code" aria-label={`配对码 ${pairing.code}`}>{pairing.code.slice(0, 4)} {pairing.code.slice(4)}</div><Typography.Text type="secondary">10 分钟内有效且只能使用一次。长 Token 不会显示在网页或终端。</Typography.Text></Space>}
    </Modal>
  </section>
}
