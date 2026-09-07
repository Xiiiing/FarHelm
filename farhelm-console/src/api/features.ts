import { PROTOCOL_VERSION } from './health'

export type ExperimentState = 'watching' | 'succeeded' | 'failed' | 'unknown' | 'cancelled'
export type Experiment = { watch_id: string; agent_id: string; project_id: string; name: string; pid: number; state: ExperimentState; session_id?: string; detail?: string; updated_at_unix: number }
export type SessionState = 'creating' | 'idle' | 'queued' | 'running' | 'interrupting' | 'failed' | 'orphaned' | 'archived'
export type CodexSession = { session_id: string; agent_id: string; project_id: string; mode: 'inspect' | 'edit'; state: SessionState; title?: string; active_turn_id?: string; updated_at_unix: number }
export type TranscriptItem = { item_id: string; kind: 'user_message' | 'assistant_message' | 'command_summary' | 'file_change_summary' | 'error'; text: string; text_offset?: number; text_complete?: boolean }
export type TranscriptTurn = { turn_id: string; status: string; started_at_unix?: number; completed_at_unix?: number; items: TranscriptItem[] }
export type TranscriptPage = { protocol?: string; session_id: string; turns: TranscriptTurn[]; next_cursor?: string }
export type ScheduleTrigger = { type: 'at_time'; run_at_unix: number } | { type: 'experiment_succeeded'; watch_id: string }
export type CodexSchedule = { schedule_id: string; agent_id: string; session_id: string; project_id: string; trigger: ScheduleTrigger; state: 'pending' | 'queued' | 'running' | 'completed' | 'cancelled' | 'skipped' | 'missed' | 'failed' | 'orphaned'; created_at_unix: number; updated_at_unix: number }
export type ProjectCandidate = { candidate_id: string; agent_id: string; display_name: string; suggested_project_id: string; session_count: number; state: 'discovered' | 'approved'; updated_at_unix: number }

export async function json<T>(url: string): Promise<T> {
  const response = await fetch(url, { credentials: 'same-origin', headers: { Accept: 'application/json' } })
  if (!response.ok) throw await apiError(response)
  return response.json() as Promise<T>
}

export async function fetchExperiments(): Promise<Experiment[]> {
  const value = await json<{ protocol: string; experiments: Experiment[] }>('/api/v1/experiments')
  if (value.protocol !== PROTOCOL_VERSION || !Array.isArray(value.experiments)) throw new Error('Hub returned invalid experiments')
  return value.experiments
}

export async function fetchSessionPage(project?: string, archived: 'false' | 'true' | 'all' = 'false', cursor?: string) {
  const params = new URLSearchParams({ archived, limit: '50' }); if (project) params.set('project', project); if (cursor) params.set('cursor', cursor)
  const value = await json<{ protocol: string; sessions: CodexSession[]; next_cursor?: string }>(`/api/v1/codex/sessions?${params}`)
  if (value.protocol !== PROTOCOL_VERSION || !Array.isArray(value.sessions)) throw new Error('Hub returned invalid sessions')
  return value
}
export async function fetchSessions(project?: string, archived: 'false' | 'true' | 'all' = 'false') {
  return (await fetchSessionPage(project, archived)).sessions
}

export async function fetchTranscript(sessionId: string, cursor?: string): Promise<TranscriptPage> {
  const params = new URLSearchParams({ limit: '20' }); if (cursor) params.set('cursor', cursor)
  const value = await json<TranscriptPage>(`/api/v1/codex/sessions/${encodeURIComponent(sessionId)}/transcript?${params}`)
  if (!Array.isArray(value.turns) || value.session_id !== sessionId) throw new Error('Hub returned invalid transcript')
  return value
}

export async function fetchSchedules(sessionId?: string): Promise<CodexSchedule[]> {
  const suffix = sessionId ? `?session=${encodeURIComponent(sessionId)}` : ''
  const value = await json<{ protocol: string; schedules: CodexSchedule[] }>(`/api/v1/codex/schedules${suffix}`)
  if (value.protocol !== PROTOCOL_VERSION || !Array.isArray(value.schedules)) throw new Error('Hub returned invalid schedules')
  return value.schedules
}

export async function fetchProjects(): Promise<ProjectCandidate[]> {
  const value = await json<{ protocol: string; projects: ProjectCandidate[] }>('/api/v1/projects')
  if (value.protocol !== PROTOCOL_VERSION || !Array.isArray(value.projects)) throw new Error('Hub returned invalid projects')
  return value.projects
}

const pendingOperations = new Map<string, string>()
const savedReceipts = new Map<string, { identity: string; key: string }>()
export type Operation = { command_id?: string; state?: string; status_url?: string }
async function apiError(response: Response) {
  const value = await response.json().catch(() => ({})) as { error?: string }
  const errors: Record<string, string> = { operation_expired: '这次操作已过期，草稿已保留；核对状态后可修改指令重新提交', operation_failed: '这次操作已失败，草稿已保留；请先核对执行结果', agent_offline: 'Agent 离线，指令尚未保存；连接恢复后重试', agent_upgrade_required: '请先升级 Agent 至 V0.7.0', agent_save_unconfirmed: '尚未确认 Agent 保存，重试会核对同一次操作', invalid_schedule_time: '时间必须在 60 秒至 365 天之间', session_is_not_running: '当前会话已没有活动对话', visible_turn_changed: '活动对话已改变，请刷新后再操作', idempotency_conflict: '操作身份与之前的请求冲突' }
  return new Error(errors[value.error ?? ''] ?? (response.status === 401 ? '登录已过期，请重新登录' : `请求失败（HTTP ${response.status}）${value.error ? `：${value.error}` : ''}`))
}
export async function mutate(url: string, csrf: string, body?: unknown, method = 'POST'): Promise<Operation> {
  const identity = `${method}:${url}:${JSON.stringify(body)}`
  const key = pendingOperations.get(identity) ?? crypto.randomUUID()
  pendingOperations.set(identity, key)
  const response = await fetch(url, { method, credentials: 'same-origin', headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': csrf, 'Idempotency-Key': key }, body: body === undefined ? undefined : JSON.stringify(body) })
  if (!response.ok) {
    if ([400, 401, 403, 404].includes(response.status)) pendingOperations.delete(identity)
    throw await apiError(response)
  }
  const result = response.status === 204 ? {} : await response.json().catch(() => ({})) as Operation
  pendingOperations.delete(identity)
  if (result.command_id) { savedReceipts.set(result.command_id, { identity, key }); if (savedReceipts.size > 256) savedReceipts.delete(savedReceipts.keys().next().value!) }
  return result
}
export async function waitForCommand(operation: Operation): Promise<void> {
  if (!operation.command_id) return
  try {
    for (let count = 0; count < 60; count++) {
      const status = await json<{ state: string; detail?: string }>(`/api/v1/commands/${encodeURIComponent(operation.command_id)}`)
      if (status.state === 'completed') { savedReceipts.delete(operation.command_id); return }
      if (['failed', 'expired', 'unknown'].includes(status.state)) throw new Error(status.detail || `操作结果：${status.state}`)
      await new Promise((resolve) => setTimeout(resolve, 500))
    }
    throw new Error('Agent 已接收，操作仍在进行；重试会核对原操作')
  } catch (error) {
    const receipt = savedReceipts.get(operation.command_id)
    if (receipt) pendingOperations.set(receipt.identity, receipt.key)
    throw error
  }
}

export function createSession(csrf: string, agentId: string, projectId: string, mode: 'inspect' | 'edit') {
  return mutate('/api/v1/codex/sessions', csrf, { agent_id: agentId, project_id: projectId, mode })
}
export function sendMessage(csrf: string, sessionId: string, prompt: string, delivery: 'queue' | 'steer') {
  return mutate(`/api/v1/codex/sessions/${encodeURIComponent(sessionId)}/messages`, csrf, { prompt, delivery })
}
export function interruptSession(csrf: string, sessionId: string, turnId?: string) {
  return mutate(`/api/v1/codex/sessions/${encodeURIComponent(sessionId)}/interrupt`, csrf, { turn_id: turnId })
}
export function createSchedule(csrf: string, sessionId: string, prompt: string, trigger: ScheduleTrigger) {
  return mutate(`/api/v1/codex/sessions/${encodeURIComponent(sessionId)}/schedules`, csrf, { prompt, trigger })
}
export function cancelSchedule(csrf: string, scheduleId: string) {
  return mutate(`/api/v1/codex/schedules/${encodeURIComponent(scheduleId)}/cancel`, csrf)
}
export function importProjects(csrf: string, agentId: string, candidateIds: string[]) {
  return mutate('/api/v1/projects/import', csrf, { agent_id: agentId, candidate_ids: candidateIds })
}

function applicationServerKey(value: string): Uint8Array<ArrayBuffer> {
  const padded = value.replace(/-/g, '+').replace(/_/g, '/') + '='.repeat((4 - value.length % 4) % 4)
  const bytes = Uint8Array.from(atob(padded), (character) => character.charCodeAt(0))
  return bytes.buffer instanceof ArrayBuffer ? new Uint8Array(bytes.buffer) : new Uint8Array(bytes)
}

export function pushSupported(): boolean {
  return 'serviceWorker' in navigator && 'PushManager' in window && 'Notification' in window
}

export async function currentPushSubscription(): Promise<PushSubscription | null> {
  if (!pushSupported()) return null
  return (await navigator.serviceWorker.ready).pushManager.getSubscription()
}

export async function enablePush(csrf: string): Promise<void> {
  if (!pushSupported()) throw new Error('当前浏览器不支持 Web Push')
  const permission = await Notification.requestPermission()
  if (permission !== 'granted') throw new Error('通知权限未授权')
  const response = await fetch('/api/v1/push/public-key', { credentials: 'same-origin' })
  if (!response.ok) throw new Error('Hub 尚未配置 Web Push')
  const value = (await response.json()) as { public_key?: string }
  if (!value.public_key) throw new Error('Hub 返回了无效的 Push 公钥')
  const registration = await navigator.serviceWorker.ready
  const subscription = await registration.pushManager.getSubscription() ?? await registration.pushManager.subscribe({
    userVisibleOnly: true,
    applicationServerKey: applicationServerKey(value.public_key),
  })
  const saved = await fetch('/api/v1/push/subscriptions', {
    method: 'POST', credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': csrf },
    body: JSON.stringify(subscription.toJSON()),
  })
  if (!saved.ok) throw new Error(`Hub 保存 Push 订阅失败（HTTP ${saved.status}）`)
}

export async function disablePush(csrf: string): Promise<void> {
  const subscription = await currentPushSubscription()
  if (!subscription) return
  const removed = await fetch('/api/v1/push/subscriptions', {
    method: 'DELETE', credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': csrf },
    body: JSON.stringify({ endpoint: subscription.endpoint }),
  })
  if (!removed.ok) throw new Error(`Hub 删除 Push 订阅失败（HTTP ${removed.status}）`)
  await subscription.unsubscribe()
}
