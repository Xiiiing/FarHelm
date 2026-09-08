import { Alert, Button, Empty, List, Skeleton, Space, Tag, Typography } from 'antd'
import { useInfiniteQuery } from '@tanstack/react-query'
import { useMemo } from 'react'
import { json } from '../api/features'
import { uniqueRows } from '../api/metadata'
type Entry = { id: number; action: string; target: string; outcome: string; created_at_unix: number }
export function AuditPage() {
  const listing = useInfiniteQuery({ queryKey: ['audit'], initialPageParam: undefined as number | undefined,
    queryFn: ({ pageParam, signal }) => json<{ entries: Entry[]; next_cursor?: number }>(`/api/v1/audit${pageParam ? `?cursor=${pageParam}` : ''}`, signal),
    getNextPageParam: page => page.next_cursor ?? undefined,
  })
  const rows = useMemo(() => uniqueRows(listing.data?.pages.flatMap(page => page.entries) ?? [], row => row.id), [listing.data])
  return <section className="feature-page"><div className="page-heading"><Typography.Title level={1}>审计</Typography.Title><Button loading={listing.isFetching} onClick={() => void listing.refetch()}>刷新</Button></div>{listing.error && <Alert type="error" title="读取失败" description={listing.error.message} action={<Button onClick={() => void listing.refetch()}>重试</Button>} />}
    {listing.isPending ? <div role="status">正在读取审计记录…<Skeleton active /></div> : <List rowKey="id" dataSource={rows} locale={{ emptyText: listing.error ? '审计暂时不可用，请重试' : <Empty description="尚无操作记录" /> }} renderItem={row => <List.Item><List.Item.Meta title={<Space wrap>{row.action}<Tag>{row.outcome}</Tag></Space>} description={`${row.target} · ${new Date(row.created_at_unix * 1000).toLocaleString()}`} /></List.Item>} />}
    {listing.hasNextPage && <Button loading={listing.isFetchingNextPage} disabled={listing.isFetching} onClick={() => void listing.fetchNextPage()}>加载更多记录</Button>}
  </section>
}
