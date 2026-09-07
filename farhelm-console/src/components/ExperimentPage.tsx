import { Alert, Button, Card, Empty, List, Select, Space, Tag, Typography } from 'antd'
import { useCallback, useEffect, useRef, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { json } from '../api/features'
import { subscribeEvents } from '../api/events'
import { stateLabel } from '../api/notifications'
type Run = { id: string; watch_id?: string; agent_id: string; project_id: string; name: string; pid?: number; source: 'pid_watch' | 'script_report'; state: string; message?: string; session_id?: string; followup_schedule_id?: string; updated_at_unix: number }
export function ExperimentPage() {
  const navigate = useNavigate(); const [params] = useSearchParams(); const target = params.get('run') ?? params.get('watch')
  const [items, setItems] = useState<Run[]>([]); const [cursor, setCursor] = useState<number>(); const [source, setSource] = useState(''); const [state, setState] = useState(''); const [agent, setAgent] = useState(''); const [error, setError] = useState<string>(); const [loading, setLoading] = useState(false); const generation = useRef(0)
  const load = useCallback(async (after?: number) => {
    const request = ++generation.current; setLoading(true)
    const query = new URLSearchParams(); if (after) query.set('cursor', String(after)); if (state) query.set('state', state); if (agent) query.set('agent', agent); if (target) query.set('id', target); if (source) query.set('source', source)
    try { const page = await json<{ experiments: Run[]; next_cursor?: number }>(`/api/v1/experiment-runs?${query}`); if (generation.current === request) { setItems((old) => after ? [...old, ...page.experiments] : page.experiments); setCursor(page.next_cursor); setError(undefined) } } catch (e) { setError(e instanceof Error ? e.message : '实验不可用') } finally { if (generation.current === request) setLoading(false) }
  }, [agent, state, target, source])
  useEffect(() => { const initial = setTimeout(() => void load(), 0); const timer = setInterval(() => void load(), 15000); const off = subscribeEvents(['experiment.updated', 'experiment.reported', 'open'], () => void load()); return () => { clearTimeout(initial); clearInterval(timer); off() } }, [load])
  return <section className="feature-page" aria-labelledby="experiments-title"><div className="page-heading"><div><Typography.Title id="experiments-title" level={1}>实验</Typography.Title><Typography.Paragraph type="secondary">服务器本地登记的 PID 和脚本显式上报的结果。</Typography.Paragraph></div><Button loading={loading} onClick={() => void load()}>刷新</Button></div>
    {error && <Alert type="warning" title="无法读取实验" description={error} />}
    <Space wrap style={{ marginBottom: 16 }}>{target && <Button onClick={() => navigate('/experiments')}>查看所有实验</Button>}<Select aria-label="实验来源" value={source} onChange={setSource} options={[{value:'',label:'全部来源'},{value:'script_report',label:'脚本上报'},{value:'pid_watch',label:'PID 监控'}]} /><Select aria-label="实验状态" value={state} onChange={setState} options={['', 'watching', 'succeeded', 'failed', 'unknown'].map((value) => ({ value, label: value ? stateLabel(value) : '全部状态' }))} /><Select aria-label="实验服务器" value={agent} onChange={setAgent} options={[{ value: '', label: '全部服务器' }, ...[...new Set(items.map((item) => item.agent_id))].map((value) => ({ value, label: value }))]} /></Space>
    <Card className="agent-list-card" styles={{ body: { padding: 0 } }}><List dataSource={items} locale={{ emptyText: <Empty description="当前范围没有实验" /> }} renderItem={(item) => <List.Item className={item.id === target ? 'agent-row highlighted' : 'agent-row'}><List.Item.Meta title={<Space wrap>{item.name}<Tag>{stateLabel(item.state)}</Tag><Tag>{item.source === 'script_report' ? '脚本上报' : `PID ${item.pid}`}</Tag></Space>} description={<Space orientation="vertical"><Typography.Text type="secondary">{item.agent_id} · {item.project_id} · {new Date(item.updated_at_unix * 1000).toLocaleString()}</Typography.Text>{item.message && <span>{item.message}</span>}{item.session_id && <Button onClick={() => navigate(`/codex?session=${encodeURIComponent(item.session_id!)}`)}>打开关联 Codex{item.followup_schedule_id ? ' 与后续任务' : ''}</Button>}</Space>} /></List.Item>} /></Card>
    {cursor && <Button loading={loading} onClick={() => void load(cursor)}>加载更多实验</Button>}
  </section>
}
