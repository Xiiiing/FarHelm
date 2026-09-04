import { PROTOCOL_VERSION } from './health'

export type ExperimentState = 'watching' | 'succeeded' | 'failed' | 'unknown' | 'cancelled'
export type Experiment = { watch_id: string; agent_id: string; project_id: string; name: string; pid: number; state: ExperimentState; session_id?: string; detail?: string; updated_at_unix: number }
export type SessionState = 'creating' | 'idle' | 'queued' | 'running' | 'interrupting' | 'failed' | 'orphaned' | 'archived'
export type CodexSession = { session_id: string; agent_id: string; project_id: string; mode: 'inspect' | 'edit'; state: SessionState; title?: string; active_turn_id?: string; updated_at_unix: number }
export type ProjectCandidate = { candidate_id: string; agent_id: string; display_name: string; suggested_project_id: string; session_count: number; state: 'discovered' | 'approved'; updated_at_unix: number }

async function json<T>(url: string): Promise<T> {
  const response = await fetch(url, { credentials: 'same-origin', headers: { Accept: 'application/json' } })
  if (!response.ok) throw new Error(`Hub returned HTTP ${response.status}`)
  return response.json() as Promise<T>
}

export async function fetchExperiments(): Promise<Experiment[]> {
  const value = await json<{ protocol: string; experiments: Experiment[] }>('/api/v1/experiments')
  if (value.protocol !== PROTOCOL_VERSION || !Array.isArray(value.experiments)) throw new Error('Hub returned invalid experiments')
  return value.experiments
}

export async function fetchSessions(project?: string, archived: 'false' | 'true' | 'all' = 'false'): Promise<CodexSession[]> {
  const params = new URLSearchParams({ archived }); if (project) params.set('project', project)
  const query = `?${params.toString()}`
  const value = await json<{ protocol: string; sessions: CodexSession[] }>(`/api/v1/codex/sessions${query}`)
  if (value.protocol !== PROTOCOL_VERSION || !Array.isArray(value.sessions)) throw new Error('Hub returned invalid sessions')
  return value.sessions
}

export async function fetchProjects(): Promise<ProjectCandidate[]> {
  const value = await json<{ protocol: string; projects: ProjectCandidate[] }>('/api/v1/projects')
  if (value.protocol !== PROTOCOL_VERSION || !Array.isArray(value.projects)) throw new Error('Hub returned invalid projects')
  return value.projects
}

function idempotencyKey() { return `${Date.now()}-${crypto.randomUUID()}` }

async function mutate(url: string, csrf: string, body?: unknown): Promise<void> {
  const response = await fetch(url, {
    method: 'POST', credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': csrf, 'Idempotency-Key': idempotencyKey() },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  if (!response.ok) throw new Error(`Hub returned HTTP ${response.status}`)
}

export function createSession(csrf: string, agentId: string, projectId: string, mode: 'inspect' | 'edit') {
  return mutate('/api/v1/codex/sessions', csrf, { agent_id: agentId, project_id: projectId, mode })
}
export function sendMessage(csrf: string, sessionId: string, prompt: string, delivery: 'queue' | 'steer') {
  return mutate(`/api/v1/codex/sessions/${encodeURIComponent(sessionId)}/messages`, csrf, { prompt, delivery })
}
export function interruptSession(csrf: string, sessionId: string) {
  return mutate(`/api/v1/codex/sessions/${encodeURIComponent(sessionId)}/interrupt`, csrf)
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
