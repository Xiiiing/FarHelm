import { PlusOutlined, ReloadOutlined, SendOutlined, StopOutlined } from '@ant-design/icons'
import { Alert, Button, Card, Empty, Form, Input, List, Modal, Radio, Segmented, Select, Space, Tag, Typography } from 'antd'
import { useCallback, useEffect, useState } from 'react'
import { useSearchParams } from 'react-router-dom'

import { createSession, fetchSessions, interruptSession, sendMessage, type CodexSession } from '../api/features'

export function CodexPage({ csrf }: { csrf: string }) {
  const [searchParams] = useSearchParams(); const targetSession = searchParams.get('session'); const [sessions, setSessions] = useState<CodexSession[]>([]); const [selected, setSelected] = useState<CodexSession>(); const [error, setError] = useState<string>(); const [creating, setCreating] = useState(false); const [replies, setReplies] = useState<Record<string, string>>({}); const [projectQuery, setProjectQuery] = useState(''); const [projectFilter, setProjectFilter] = useState(''); const [archiveFilter, setArchiveFilter] = useState<'false' | 'true' | 'all'>('false'); const [form] = Form.useForm()
  const load = useCallback(() => { void fetchSessions(projectFilter || undefined, archiveFilter).then((items) => { setSessions(items); setSelected((current) => items.find((item) => item.session_id === (current?.session_id ?? targetSession)) ?? current) }).catch((reason: unknown) => setError(reason instanceof Error ? reason.message : 'Codex 会话不可用')) }, [archiveFilter, projectFilter, targetSession])
  useEffect(() => {
    load(); const stream = new EventSource('/api/v1/events/stream')
    stream.addEventListener('codex.session.updated', load)
    stream.addEventListener('codex.turn.completed', load)
    stream.addEventListener('codex.message.delta', (event) => {
      try {
        const envelope = JSON.parse((event as MessageEvent<string>).data) as { payload?: { session_id?: string; data?: { delta?: string } } }
        const sessionId = envelope.payload?.session_id; const delta = envelope.payload?.data?.delta
        if (sessionId && delta) setReplies((current) => ({ ...current, [sessionId]: `${current[sessionId] ?? ''}${delta}` }))
      } catch { /* malformed stream items are ignored and recovered by reconnect */ }
    })
    return () => stream.close()
  }, [load])
  const submit = async (values: { prompt: string; delivery: 'queue' | 'steer' }) => { if (!selected) return; try { await sendMessage(csrf, selected.session_id, values.prompt, values.delivery); form.resetFields(['prompt']); load() } catch (reason) { setError(reason instanceof Error ? reason.message : '发送失败') } }
  return <section className="feature-page" aria-labelledby="codex-title"><div className="page-heading"><div><Typography.Text className="eyebrow">CODEX THREADS</Typography.Text><Typography.Title id="codex-title" level={1}>Codex</Typography.Title><Typography.Paragraph type="secondary">恢复旧会话，或在只读项目/隔离 worktree 中创建新会话。</Typography.Paragraph></div><Space wrap><Segmented aria-label="会话归档筛选" value={archiveFilter} onChange={(value) => setArchiveFilter(value as 'false' | 'true' | 'all')} options={[{ label: '当前', value: 'false' }, { label: '已归档', value: 'true' }, { label: '全部', value: 'all' }]} /><Input.Search aria-label="按项目筛选会话" placeholder="项目 ID" value={projectQuery} onChange={(event) => setProjectQuery(event.target.value)} onSearch={(value) => setProjectFilter(value.trim())} /><Button icon={<ReloadOutlined />} onClick={load}>刷新</Button><Button type="primary" icon={<PlusOutlined />} onClick={() => setCreating(true)}>新会话</Button></Space></div>
    {error && <Alert showIcon type="warning" closable onClose={() => setError(undefined)} title="Codex 操作失败" description={error} />}
    <div className="codex-grid"><Card title="会话" className="session-list" styles={{ body: { padding: 0 } }}>{sessions.length === 0 ? <Empty description="尚无关联会话" /> : <List dataSource={sessions} renderItem={(item) => <List.Item className={selected?.session_id === item.session_id ? 'session-row selected' : 'session-row'} onClick={() => setSelected(item)}><List.Item.Meta title={item.title || item.session_id} description={`${item.project_id} · ${item.mode}`} /><Tag>{item.state}</Tag></List.Item>} />}</Card>
      <Card title={selected ? selected.title || selected.session_id : '回复'}>{selected ? <><Space wrap><Tag>{selected.state}</Tag><Typography.Text code>{selected.session_id}</Typography.Text></Space><div className="stream-panel" aria-live="polite">{replies[selected.session_id] || '等待回复。断线重连后，Hub 会从事件游标继续补发。'}</div><Form form={form} layout="vertical" initialValues={{ delivery: 'queue' }} onFinish={submit}><Form.Item name="prompt" label="下一条指令" rules={[{ required: true }, { max: 32768 }]}><Input.TextArea autoSize={{ minRows: 4, maxRows: 10 }} /></Form.Item><Form.Item name="delivery"><Radio.Group><Radio value="queue">排队</Radio><Radio value="steer" disabled={selected.state !== 'running'}>Steer 当前 turn</Radio></Radio.Group></Form.Item><Space><Button type="primary" htmlType="submit" icon={<SendOutlined />}>发送</Button><Button danger icon={<StopOutlined />} disabled={selected.state !== 'running'} onClick={() => void interruptSession(csrf, selected.session_id).then(load)}>中断</Button></Space></Form></> : <Empty description="选择一个会话" />}</Card></div>
    <Modal title="创建 Codex 会话" open={creating} onCancel={() => setCreating(false)} footer={null}><Form layout="vertical" onFinish={(values: { agent: string; project: string; mode: 'inspect' | 'edit' }) => void createSession(csrf, values.agent, values.project, values.mode).then(() => { setCreating(false); load() }).catch((reason: unknown) => setError(reason instanceof Error ? reason.message : '创建失败'))}><Form.Item label="Agent ID" name="agent" rules={[{ required: true }]}><Input /></Form.Item><Form.Item label="项目 ID" name="project" rules={[{ required: true }]}><Input /></Form.Item><Form.Item label="模式" name="mode" initialValue="inspect"><Select options={[{ value: 'inspect', label: 'Inspect（项目只读）' }, { value: 'edit', label: 'Edit（隔离 Git worktree）' }]} /></Form.Item><Button type="primary" htmlType="submit" block>创建</Button></Form></Modal>
  </section>
}
