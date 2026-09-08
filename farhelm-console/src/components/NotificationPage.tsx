import { Alert, Badge, Button, Drawer, Empty, List, Select, Skeleton, Space, Tag, Typography } from 'antd'
import { useInfiniteQuery, useMutation, useQuery } from '@tanstack/react-query'
import { useEffect, useMemo, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { queryClient } from '../api/cache'
import { uniqueRows } from '../api/metadata'
import { markAllRead, markRead, noticeDetail, notices, stateLabel, type NoticeDetail } from '../api/notifications'
import { useAgents } from '../hooks/useAgents'
import { NotificationSettings } from './NotificationSettings'

export function NotificationPage({ csrf }: { csrf: string }) {
  const navigate = useNavigate(); const [params, setParams] = useSearchParams()
  const [category, setCategory] = useState(''); const [state, setState] = useState(''); const [agent, setAgent] = useState(''); const [unread, setUnread] = useState(false)
  const { agents } = useAgents()
  const listing = useInfiniteQuery({
    queryKey: ['notifications', 'list', { category, state, agent, unread }], initialPageParam: undefined as number | undefined,
    queryFn: ({ pageParam, signal }) => {
      const query = new URLSearchParams(); if (category) query.set('category', category); if (state) query.set('state', state); if (agent) query.set('agent', agent); if (unread) query.set('unread', 'true'); if (pageParam) query.set('cursor', String(pageParam))
      return notices(query, signal)
    },
    getNextPageParam: page => page.next_cursor ?? undefined, refetchInterval: 15_000,
  })
  const items = useMemo(() => uniqueRows(listing.data?.pages.flatMap(page => page.notifications) ?? [], item => item.id), [listing.data])
  const page = listing.data?.pages[0]
  const target = params.get('id'), id = Number(target)
  const validTarget = !!target && Number.isSafeInteger(id) && id > 0
  const detail = useQuery({ queryKey: ['notifications', 'detail', id], enabled: validTarget, queryFn: ({ signal }) => noticeDetail(id, signal) })
  const read = useMutation({
    mutationFn: (target: number) => markRead(csrf, target),
    onSuccess: (_, target) => {
      queryClient.setQueryData<NoticeDetail>(['notifications', 'detail', target], old => old && ({ ...old, notification: { ...old.notification, read_at_unix: Math.floor(Date.now() / 1000) } }))
      void queryClient.invalidateQueries({ queryKey: ['notifications'] })
    },
  })
  const mark = read.mutate, detailId = detail.data?.notification.id, readAt = detail.data?.notification.read_at_unix
  useEffect(() => { if (validTarget && detailId === id && !readAt) mark(id) }, [validTarget, id, detailId, readAt, mark])
  const all = useMutation({ mutationFn: (through: number) => markAllRead(csrf, through), onSuccess: () => queryClient.invalidateQueries({ queryKey: ['notifications'] }) })
  return <section className="feature-page" aria-labelledby="notifications-title">
    <div className="page-heading"><Typography.Title id="notifications-title" level={1}>通知</Typography.Title><Space><Badge count={page?.unread_count ?? 0} showZero /><Button loading={all.isPending} disabled={!page || !page.unread_count} onClick={() => all.mutate(page!.latest_id)}>全部已读</Button><Button loading={listing.isFetching} onClick={() => void listing.refetch()}>刷新</Button></Space></div>
    {listing.error && <Alert showIcon type="error" title="无法读取通知" description={listing.error.message} action={<Button onClick={() => void listing.refetch()}>重试</Button>} />}
    {all.error && <Alert showIcon type="error" title="标记失败" description={all.error.message} />}
    <Space wrap style={{ marginBottom: 16 }}>
      <Select aria-label="通知类型" value={category} onChange={setCategory} options={[{ value: '', label: '全部类型' }, { value: 'experiment', label: '实验' }, { value: 'codex', label: 'Codex' }]} />
      <Select aria-label="通知结果" value={state} onChange={setState} options={['', 'succeeded', 'failed', 'unknown'].map(value => ({ value, label: value ? stateLabel(value) : '全部结果' }))} />
      <Select aria-label="通知服务器" value={agent} onChange={setAgent} options={[{ value: '', label: '全部服务器' }, ...(agents.data?.agents ?? []).map(value => ({ value: value.agent_id, label: value.hostname || value.agent_id }))]} />
      <Button type={unread ? 'primary' : 'default'} aria-pressed={unread} onClick={() => setUnread(!unread)}>仅未读</Button>
    </Space>
    {listing.isPending ? <div role="status">正在读取通知…<Skeleton active /></div> : <List rowKey="id" dataSource={items} locale={{ emptyText: listing.error ? '通知暂时不可用，请重试' : <Empty description="当前范围没有通知" /> }} renderItem={item => <List.Item actions={[<Button key="detail" onClick={() => setParams({ id: String(item.id) })}>查看详情</Button>]}><List.Item.Meta title={<Space wrap>{!item.read_at_unix && <Tag>未读</Tag>}{item.title}<Tag>{stateLabel(item.state)}</Tag></Space>} description={`${item.agent_id} · ${new Date(item.created_at_unix * 1000).toLocaleString()}`} /></List.Item>} />}
    {listing.hasNextPage && <Button loading={listing.isFetchingNextPage} disabled={listing.isFetching} onClick={() => void listing.fetchNextPage()}>加载更多通知</Button>}
    <NotificationSettings csrf={csrf} />
    <Drawer title="通知详情" open={!!target} onClose={() => setParams({})}>
      {!validTarget ? <Alert type="error" title="通知地址无效" /> : detail.isPending ? <div role="status">正在读取通知详情…<Skeleton active /></div> : <>
        {detail.error && <Alert type="error" title="无法读取通知详情" description={detail.error.message} action={<Button onClick={() => void detail.refetch()}>重试读取</Button>} />}
        {detail.data && <><Typography.Title level={2}>{detail.data.notification.title}</Typography.Title><Typography.Paragraph>{detail.data.notification.message || stateLabel(detail.data.notification.state)}</Typography.Paragraph>
          {read.error && read.variables === id && <Alert showIcon type="warning" title="尚未标记已读" description={read.error.message} action={<Button loading={read.isPending} onClick={() => read.mutate(id)}>重试标记</Button>} />}
          {detail.data.notification.category !== 'test' && <Button onClick={() => navigate(detail.data.notification.category === 'codex' ? `/codex?session=${encodeURIComponent(detail.data.notification.target_id)}` : `/experiments?run=${encodeURIComponent(detail.data.notification.target_id)}`)}>打开关联记录</Button>}
        </>}
      </>}
    </Drawer>
  </section>
}
