import type { CodexSession } from '../../api/features'

export const stateNames: Record<string, string> = { creating: '创建中', idle: '空闲', queued: '排队中', running: '进行中', inProgress: '进行中', interrupting: '中断中', failed: '失败', orphaned: '结果未知', archived: '已归档', completed: '已完成', interrupted: '已中断', accepted: 'Agent 已保存', delivered: '等待保存确认', submitting: '正在提交', pending: '等待触发', cancelled: '已取消', skipped: '已跳过', missed: '已错过', expired: '已过期', unknown: '结果未知', unconfirmed: '尚未确认保存', rejected: '提交未接受' }
export function formalTitle(value?: string): string | undefined {
  const title = value?.trim()
  return title && !['codex session', 'untitled', 'new conversation', 'new chat', '未命名会话', '新会话'].includes(title.toLowerCase()) ? title : undefined
}
export function sessionName(session: CodexSession): string {
  const title = formalTitle(session.title)
  if (title) return title
  if (session.display_label?.trim()) return session.display_label
  const date = new Date(session.updated_at_unix * 1000).toLocaleString('zh-CN', { month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit', hour12: false })
  return `${session.project_id} · ${date} · ${session.session_id.slice(-8)}`
}
export function errorText(error: unknown): string { return error instanceof Error ? error.message : '请求失败，请重试' }
