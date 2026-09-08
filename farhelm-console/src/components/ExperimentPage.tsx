import { Alert, Button, Card, Empty, List, Select, Skeleton, Space, Tag, Typography } from 'antd'
import { useInfiniteQuery } from '@tanstack/react-query'
import { useMemo, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { json } from '../api/features'
import { uniqueRows } from '../api/metadata'
import { stateLabel } from '../api/notifications'
import { useAgents } from '../hooks/useAgents'

type Run = { id: string; watch_id?: string; agent_id: string; project_id: string; name: string; pid?: number; source: 'pid_watch' | 'script_report'; state: string; message?: string; session_id?: string; followup_schedule_id?: string; updated_at_unix: number }
export function ExperimentPage() {
  const navigate = useNavigate(); const [params] = useSearchParams(); const target = params.get('run') ?? params.get('watch')
  const [source, setSource] = useState(''); const [state, setState] = useState(''); const [agent, setAgent] = useState('')
  const { agents } = useAgents()
  const listing = useInfiniteQuery({
    queryKey: ['experiment-runs', { agent, state, target, source }], initialPageParam: undefined as number | undefined,
    queryFn: ({ pageParam, signal }) => {
      const query = new URLSearchParams(); if (pageParam) query.set('cursor', String(pageParam)); if (state) query.set('state', state); if (agent) query.set('agent', agent); if (target) query.set('id', target); if (source) query.set('source', source)
      return json<{ experiments: Run[]; next_cursor?: number }>(`/api/v1/experiment-runs?${query}`, signal)
    },
    getNextPageParam: page => page.next_cursor ?? undefined, refetchInterval: 15_000,
  })
  const items = useMemo(() => uniqueRows(listing.data?.pages.flatMap(page => page.experiments) ?? [], item => JSON.stringify([item.agent_id, item.source, item.id])), [listing.data])
  return <section className="feature-page" aria-labelledby="experiments-title"><div className="page-heading"><div><Typography.Title id="experiments-title" level={1}>实验</Typography.Title><Typography.Paragraph type="secondary">服务器本地登记的 PID 和脚本显式上报的结果。</Typography.Paragraph></div><Button loading={listing.isFetching} onClick={() => void listing.refetch()}>刷新</Button></div>
    {listing.error && <Alert showIcon type="warning" title="无法读取实验" description={listing.error.message} action={<Button onClick={() => void listing.refetch()}>重试</Button>} />}
    <Space wrap style={{ marginBottom: 16 }}>{target && <Button onClick={() => navigate('/experiments')}>查看所有实验</Button>}<Select aria-label="实验来源" value={source} onChange={setSource} options={[{value:'',label:'全部来源'},{value:'script_report',label:'脚本上报'},{value:'pid_watch',label:'PID 监控'}]} /><Select aria-label="实验状态" value={state} onChange={setState} options={['', 'watching', 'succeeded', 'failed', 'unknown', 'cancelled'].map(value => ({ value, label: value ? stateLabel(value) : '全部状态' }))} /><Select aria-label="实验服务器" value={agent} onChange={setAgent} options={[{ value: '', label: '全部服务器' }, ...(agents.data?.agents ?? []).map(value => ({ value: value.agent_id, label: value.hostname || value.agent_id }))]} /></Space>
    {listing.isPending ? <div role="status">正在读取实验…<Skeleton active /></div> : <Card className="agent-list-card" styles={{ body: { padding: 0 } }}><List rowKey={item => JSON.stringify([item.agent_id, item.source, item.id])} dataSource={items} locale={{ emptyText: listing.error ? '实验暂时不可用，请重试' : <Empty description="当前范围没有实验" /> }} renderItem={item => <List.Item className={item.id === target ? 'agent-row highlighted' : 'agent-row'}><List.Item.Meta title={<Space wrap>{item.name}<Tag>{stateLabel(item.state)}</Tag><Tag>{item.source === 'script_report' ? '脚本上报' : `PID ${item.pid}`}</Tag></Space>} description={<Space orientation="vertical"><Typography.Text type="secondary">{item.agent_id} · {item.project_id} · {new Date(item.updated_at_unix * 1000).toLocaleString()}</Typography.Text>{item.message && <span>{item.message}</span>}{item.session_id && <Button onClick={() => navigate(`/codex?session=${encodeURIComponent(item.session_id!)}`)}>打开关联 Codex{item.followup_schedule_id ? ' 与后续任务' : ''}</Button>}</Space>} /></List.Item>} /></Card>}
    {listing.hasNextPage && <Button loading={listing.isFetchingNextPage} disabled={listing.isFetching} onClick={() => void listing.fetchNextPage()}>加载更多实验</Button>}
  </section>
}
