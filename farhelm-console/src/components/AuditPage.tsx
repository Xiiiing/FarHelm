import { Alert, Button, Empty, List, Space, Tag, Typography } from 'antd'
import { useCallback, useEffect, useState } from 'react'
import { json } from '../api/features'
type Entry = { id: number; action: string; target: string; outcome: string; created_at_unix: number }
export function AuditPage() {
  const [rows, setRows] = useState<Entry[]>([]); const [cursor, setCursor] = useState<number>(); const [error, setError] = useState<string>(); const [busy, setBusy] = useState(false)
  const load = useCallback(async (after?: number) => {
    setBusy(true)
    try { const page = await json<{ entries: Entry[]; next_cursor?: number }>(`/api/v1/audit${after ? `?cursor=${after}` : ''}`); setRows((old) => after ? [...old, ...page.entries] : page.entries); setCursor(page.next_cursor) } catch (e) { setError(e instanceof Error ? e.message : '审计不可用') } finally { setBusy(false) }
  }, [])
  useEffect(() => { const initial = setTimeout(() => void load(), 0); return () => clearTimeout(initial) }, [load])
  return <section className="feature-page"><div className="page-heading"><Typography.Title level={1}>审计</Typography.Title><Button loading={busy} onClick={() => void load()}>刷新</Button></div>{error && <Alert type="error" title="读取失败" description={error} />}<List dataSource={rows} locale={{ emptyText: <Empty description="尚无操作记录" /> }} renderItem={(row) => <List.Item><List.Item.Meta title={<Space>{row.action}<Tag>{row.outcome}</Tag></Space>} description={`${row.target} · ${new Date(row.created_at_unix * 1000).toLocaleString()}`} /></List.Item>} />{cursor && <Button loading={busy} onClick={() => void load(cursor)}>加载更多记录</Button>}</section>
}
