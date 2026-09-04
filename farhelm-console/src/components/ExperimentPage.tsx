import { ReloadOutlined } from '@ant-design/icons'
import { Alert, Button, Card, Empty, List, Space, Tag, Typography } from 'antd'
import { useCallback, useEffect, useState } from 'react'
import { useSearchParams } from 'react-router-dom'

import { fetchExperiments, type Experiment } from '../api/features'

const colors = { watching: 'processing', succeeded: 'success', failed: 'error', unknown: 'warning', cancelled: 'default' } as const

export function ExperimentPage() {
  const [searchParams] = useSearchParams(); const targetWatch = searchParams.get('watch')
  const [items, setItems] = useState<Experiment[]>([]); const [error, setError] = useState<string>(); const [loading, setLoading] = useState(true)
  const load = useCallback(() => { setLoading(true); void fetchExperiments().then(setItems).catch((reason: unknown) => setError(reason instanceof Error ? reason.message : '实验不可用')).finally(() => setLoading(false)) }, [])
  useEffect(() => { const timer = window.setTimeout(load, 0); const stream = new EventSource('/api/v1/events/stream'); stream.addEventListener('experiment.updated', load); return () => { window.clearTimeout(timer); stream.close() } }, [load])
  return <section className="feature-page" aria-labelledby="experiments-title"><div className="page-heading"><div><Typography.Text className="eyebrow">REGISTERED PROCESSES</Typography.Text><Typography.Title id="experiments-title" level={1}>实验</Typography.Title><Typography.Paragraph type="secondary">仅显示在服务器本地 CLI 登记的 PID；这里不能启停训练。</Typography.Paragraph></div><Button icon={<ReloadOutlined />} onClick={load} loading={loading}>刷新</Button></div>
    {error && <Alert showIcon type="warning" title="无法读取实验" description={error} />}
    <Card className="agent-list-card" styles={{ body: { padding: 0 } }}>{items.length === 0 ? <Empty description="尚未登记实验" /> : <List dataSource={items} renderItem={(item) => <List.Item className={item.watch_id === targetWatch ? 'agent-row highlighted' : 'agent-row'}><List.Item.Meta title={<Space wrap><span>{item.name}</span><Tag color={colors[item.state]}>{item.state}</Tag></Space>} description={`${item.agent_id} · ${item.project_id} · PID ${item.pid}`} /><div className="experiment-detail"><span>{item.detail ?? '正在观察进程'}</span><time>{new Date(item.updated_at_unix * 1000).toLocaleString()}</time></div></List.Item>} />}</Card>
  </section>
}
