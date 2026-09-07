import { Alert, Badge, Button, Drawer, Empty, List, Select, Space, Tag, Typography } from 'antd'
import { useCallback, useEffect, useRef, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { subscribeEvents } from '../api/events'
import { markAllRead, markRead, noticeDetail, notices, stateLabel, type Delivery, type Notice, type NoticePage } from '../api/notifications'
import { NotificationSettings } from './NotificationSettings'

export function NotificationPage({ csrf }: { csrf: string }) {
  const navigate = useNavigate(); const [params, setParams] = useSearchParams()
  const [page, setPage] = useState<NoticePage>(); const [category, setCategory] = useState(''); const [state, setState] = useState(''); const [agent, setAgent] = useState(''); const [unread, setUnread] = useState(false)
  const [detail, setDetail] = useState<{ notification: Notice; deliveries: Delivery[] }>(); const [error, setError] = useState<string>(); const [busy, setBusy] = useState(false); const generation = useRef(0)
  const load = useCallback(async (cursor?: number) => {
    const current = ++generation.current; setBusy(true)
    try {
      const query = new URLSearchParams(); if (category) query.set('category', category); if (state) query.set('state', state); if (agent) query.set('agent', agent); if (unread) query.set('unread', 'true'); if (cursor) query.set('cursor', String(cursor))
      const result = await notices(query)
      if (generation.current === current) setPage((previous) => cursor && previous ? { ...result, notifications: [...previous.notifications, ...result.notifications] } : result)
    } catch (e) { setError(e instanceof Error ? e.message : '无法读取通知') } finally { if (generation.current === current) setBusy(false) }
  }, [agent, category, state, unread])
  useEffect(() => {
    const initial = setTimeout(() => void load(), 0); const timer = setInterval(() => void load(), 15000)
    const off = subscribeEvents(['notification.changed', 'open', 'experiment.updated', 'experiment.reported', 'codex.turn.completed', 'codex.turn.failed', 'codex.turn.orphaned'], () => void load())
    return () => { clearTimeout(initial); clearInterval(timer); off() }
  }, [load])
  const target = params.get('id')
  useEffect(() => {
    let cancelled = false
    if (target) void noticeDetail(Number(target)).then(async (value) => { if (!cancelled) { setDetail(value); await markRead(csrf, value.notification.id); void load() } }).catch((e: unknown) => setError(e instanceof Error ? e.message : '通知不可用'))
    return () => { cancelled = true }
  }, [csrf, target, load])
  const markAll = async () => { try { await markAllRead(csrf, page?.latest_id ?? 0); await load() } catch (e) { setError(e instanceof Error ? e.message : '标记失败') } }
  return <section className="feature-page" aria-labelledby="notifications-title">
    <div className="page-heading"><Typography.Title id="notifications-title" level={1}>通知</Typography.Title><Space><Badge count={page?.unread_count ?? 0} showZero /><Button onClick={() => void markAll()}>全部已读</Button><Button loading={busy} onClick={() => void load()}>刷新</Button></Space></div>
    {error && <Alert showIcon type="error" title="通知操作失败" description={error} />}
    <Space wrap style={{ marginBottom: 16 }}>
      <Select aria-label="通知类型" value={category} onChange={setCategory} options={[{ value: '', label: '全部类型' }, { value: 'experiment', label: '实验' }, { value: 'codex', label: 'Codex' }]} />
      <Select aria-label="通知结果" value={state} onChange={setState} options={['', 'succeeded', 'failed', 'unknown'].map((value) => ({ value, label: value ? stateLabel(value) : '全部结果' }))} />
      <Select aria-label="通知服务器" value={agent} onChange={setAgent} options={[{ value: '', label: '全部服务器' }, ...[...new Set(page?.notifications.map((n) => n.agent_id))].map((value) => ({ value, label: value }))]} />
      <Button type={unread ? 'primary' : 'default'} onClick={() => setUnread(!unread)}>仅未读</Button>
    </Space>
    <List dataSource={page?.notifications ?? []} locale={{ emptyText: <Empty description="当前范围没有通知" /> }} renderItem={(item) => <List.Item actions={[<Button key="detail" onClick={() => setParams({ id: String(item.id) })}>查看详情</Button>]}><List.Item.Meta title={<Space>{!item.read_at_unix && <Tag>未读</Tag>}{item.title}<Tag>{stateLabel(item.state)}</Tag></Space>} description={`${item.agent_id} · ${new Date(item.created_at_unix * 1000).toLocaleString()}`} /></List.Item>} />
    {page?.next_cursor && <Button loading={busy} onClick={() => void load(page.next_cursor)}>加载更多通知</Button>}
    <NotificationSettings csrf={csrf} />
    <Drawer title="通知详情" open={Boolean(target && detail && detail.notification.id === Number(target))} onClose={() => setParams({})}><Typography.Title level={2}>{detail?.notification.title}</Typography.Title><Typography.Paragraph>{detail?.notification.message || stateLabel(detail?.notification.state ?? '')}</Typography.Paragraph>
      {detail?.notification.category !== 'test' && <Button onClick={() => { if (detail) navigate(detail.notification.category === 'codex' ? `/codex?session=${encodeURIComponent(detail.notification.target_id)}` : `/experiments?run=${encodeURIComponent(detail.notification.target_id)}`) }}>打开关联记录</Button>}
    </Drawer>
  </section>
}
