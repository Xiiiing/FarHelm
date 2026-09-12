import { useQuery } from '@tanstack/react-query'
import { Alert, Progress, Space, Steps, Tag } from 'antd'
import { fetchNativeControl } from '../../api/features'

type Activity = { plan?: { step: string; status: string }[]; usage?: { total?: { totalTokens?: number }; modelContextWindow?: number }; goal?: { objective?: string; status?: string; tokensUsed?: number; tokenBudget?: number }; warning?: string; model?: string }
export function NativeActivity({ session, enabled }: { session: string; enabled: boolean }) {
  const activity = useQuery({ queryKey: ['codex', 'native', session, 'activity'], enabled, queryFn: ({ signal }) => fetchNativeControl<Activity>(session, 'activity', signal) })
  const data = activity.data?.data
  if (!data || !enabled) return null
  return <Space direction="vertical" className="native-control-stack">
    {!!data.plan?.length && <Steps size="small" direction="vertical" items={data.plan.map(item => ({ title: item.step, status: item.status === 'completed' ? 'finish' : item.status === 'inProgress' ? 'process' : 'wait' }))} />}
    {data.goal?.objective && <Alert type="info" title={data.goal.objective} description={<><span>目标：{data.goal.status} · {data.goal.tokensUsed ?? 0} Token</span>{!!data.goal.tokenBudget && <Progress percent={Math.min(100, Math.round((data.goal.tokensUsed ?? 0) / data.goal.tokenBudget * 100))} />}</>} />}
    {typeof data.usage?.total?.totalTokens === 'number' && <span>Token 用量：{data.usage.total.totalTokens.toLocaleString()}{data.usage.modelContextWindow ? ` · 上下文窗口 ${data.usage.modelContextWindow.toLocaleString()}` : ''}</span>}
    {data.model && <Tag>模型已切换为 {data.model}</Tag>}
    {data.warning && <Alert type="warning" title={data.warning} />}
  </Space>
}
