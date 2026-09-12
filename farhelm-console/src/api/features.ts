import { QueryObserver } from '@tanstack/react-query'
import { queryClient, keys, cacheOperation } from './cache'
import { PROTOCOL_VERSION } from './health'

export type ExperimentState = 'watching' | 'succeeded' | 'failed' | 'unknown' | 'cancelled'
export type Experiment = { watch_id: string; agent_id: string; project_id: string; name: string; pid: number; state: ExperimentState; session_id?: string; detail?: string; updated_at_unix: number }
export type SessionState = 'creating' | 'idle' | 'queued' | 'running' | 'interrupting' | 'failed' | 'orphaned' | 'archived'
export type CodexSession = { session_id: string; agent_id: string; project_id: string; mode: 'inspect' | 'edit'; state: SessionState; title?: string; display_label?: string; is_pinned?: boolean; section_id?: string; section_name?: string; active_turn_id?: string; updated_at_unix: number; revision?: number }
export type TranscriptItem = { client_id?: string; item_id: string; kind: 'user_message' | 'assistant_message' | 'command_summary' | 'file_change_summary' | 'image' | 'error'; text: string; text_offset?: number; text_complete?: boolean; status?: string; exit_code?: number; duration_ms?: number; image_resource_id?: string; mime_type?: string; streaming?: boolean }
export type TranscriptTurn = { turn_id: string; status: string; started_at_unix?: number; completed_at_unix?: number; items: TranscriptItem[] }
export type SessionContext = { ephemeral?: boolean; model?: string; reasoning_effort?: string; sandbox?: string; approval_policy?: string; approvals_reviewer?: string }
export type ModelChoice = { model: string; reasoning_effort: string }
export type ModelOption = { model: string; display_name: string; reasoning_efforts: string[]; default_reasoning_effort: string; is_default: boolean }
export type NativeIdentity = { session_id: string; persisted: boolean; held_by_agent?: boolean; native_name?: string; source?: string; history_mode?: string }
export function fetchModels(sessionId: string, signal?: AbortSignal) { return json<{ models: ModelOption[] }>(`/api/v1/codex/sessions/${encodeURIComponent(sessionId)}/models`, signal) }
export function fetchNativeIdentity(sessionId: string, signal?: AbortSignal) { return json<NativeIdentity>(`/api/v1/codex/sessions/${encodeURIComponent(sessionId)}/native`, signal) }
export function renameSession(csrf: string, sessionId: string, name: string) { return mutate(`/api/v1/codex/sessions/${encodeURIComponent(sessionId)}/name`, csrf, { name }) }
export type NativeReadKind = 'activity' | 'queue' | 'sections' | 'goal' | 'skills' | 'settings' | 'permissions' | 'delete_impact' | 'pending_interactions' | 'image_resource'
export type NativeRead<T = unknown> = { revision: string; data: T }
export function fetchNativeControl<T = unknown>(sessionId: string, kind: NativeReadKind, signal?: AbortSignal, forceReload = false) {
  return json<NativeRead<T>>(`/api/v1/codex/sessions/${encodeURIComponent(sessionId)}/native-control?${new URLSearchParams({ kind, ...(forceReload ? { force_reload: 'true' } : {}) })}`, signal)
}
export async function fetchNativeImage(sessionId: string, resourceId: string, signal?: AbortSignal) {
  const chunks: Uint8Array[] = []; let offset = 0
  for (;;) {
    const params = new URLSearchParams({ kind: 'image_resource', resource_id: resourceId, offset: String(offset) })
    const value = await json<{ next_offset: number; eof: boolean; mime_type: string; data_base64: string }>(`/api/v1/codex/sessions/${encodeURIComponent(sessionId)}/native-control?${params}`, signal)
    const binary = atob(value.data_base64); chunks.push(Uint8Array.from(binary, (character) => character.charCodeAt(0)))
    if (value.eof) return URL.createObjectURL(new Blob(chunks as BlobPart[], { type: value.mime_type }))
    if (!Number.isSafeInteger(value.next_offset) || value.next_offset <= offset) throw new Error('图片分片无效'); offset = value.next_offset
  }
}
export type NativeOperation =
  | { operation: 'queue_add'; input: ({ type: 'text'; text: string } | { type: 'skill'; skill_id: string } | { type: 'image'; attachment_id: string })[]; client_message_id: string }
  | { operation: 'queue_update'; submission_id: string; input: ({ type: 'text'; text: string } | { type: 'skill'; skill_id: string } | { type: 'image'; attachment_id: string })[]; revision: string }
  | { operation: 'temporary_start' | 'temporary_end' }
  | { operation: 'pin'; pinned: boolean }
  | { operation: 'queue_delete'; submission_id: string; revision: string }
  | { operation: 'queue_reorder'; submission_ids: string[]; revision: string }
  | { operation: 'queue_start'; submission_id?: string; revision: string }
  | { operation: 'section_create'; name: string }
  | { operation: 'section_rename'; section_id: string; name: string }
  | { operation: 'section_delete'; section_id: string }
  | { operation: 'section_move'; section_id?: string; before_thread_id?: string }
  | { operation: 'compact' }
  | { operation: 'fork'; last_turn_id: string; ephemeral: boolean }
  | { operation: 'revert'; before_turn_id: string }
  | { operation: 'delete'; impact_fingerprint: string }
  | { operation: 'goal_set'; objective?: string; status?: 'active' | 'paused' | 'blocked' | 'usage_limited' | 'budget_limited' | 'complete'; token_budget?: number }
  | { operation: 'goal_clear' }
  | { operation: 'thread_settings'; settings: { model?: string; reasoning_effort?: string; approval_policy?: string; approvals_reviewer?: string; collaboration_mode?: string; permissions?: string; service_tier?: string } }
  | { operation: 'turn_settings'; turn_id: string; settings: { model?: string; reasoning_effort?: string; approvals_reviewer?: string; service_tier?: string } }
  | { operation: 'review'; target: { type: 'uncommitted_changes' } | { type: 'base_branch'; branch: string } | { type: 'commit'; sha: string } }
  | { operation: 'interaction_answer'; request_token: string; answer: unknown }
  | { operation: 'attachment_begin'; attachment_id: string; mime_type: string; size_bytes: number; ephemeral: boolean }
  | { operation: 'attachment_chunk'; attachment_id: string; offset: number; data_base64: string }
  | { operation: 'attachment_finish'; attachment_id: string }
export function operateNative(csrf: string, sessionId: string, operation: NativeOperation) {
  const key = operation.operation === 'queue_add' ? operation.client_message_id : crypto.randomUUID()
  return mutate(`/api/v1/codex/sessions/${encodeURIComponent(sessionId)}/native-control`, csrf, { idempotency_key: key, ...operation }, 'POST', key)
}
export type TranscriptPage = { older_loaded?: boolean; protocol?: string; session_id: string; turns: TranscriptTurn[]; context?: SessionContext; next_cursor?: string; continuation?: { kind: 'message' | 'history'; turn_id: string; item_id: string; text_offset: number } }
export type DisplayPage = { sessions: CodexSession[]; next_cursor?: string; incomplete_agents: { agent_id: string; reason: string }[] }
export async function fetchSessionDisplay(csrf: string, request: { mode: 'labels' | 'search'; session_ids?: string[]; query?: string; agent_id?: string; project_id?: string; archived?: 'false' | 'true' | 'all'; cursor?: string; visible_only?: boolean }, signal?: AbortSignal): Promise<DisplayPage> {
  const response = await fetch('/api/v1/codex/session-display', { method: 'POST', credentials: 'same-origin', signal, headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': csrf }, body: JSON.stringify(request) })
  if (!response.ok) throw await apiError(response)
  const page = await response.json() as DisplayPage
  if (!Array.isArray(page.sessions) || !Array.isArray(page.incomplete_agents)) throw new Error('会话显示信息无效')
  return page
}
export type ScheduleTrigger = { type: 'at_time'; run_at_unix: number } | { type: 'experiment_succeeded'; watch_id: string }
export type CodexSchedule = { schedule_id: string; agent_id: string; session_id: string; project_id: string; trigger: ScheduleTrigger; state: 'pending' | 'queued' | 'running' | 'completed' | 'cancelled' | 'skipped' | 'missed' | 'failed' | 'orphaned'; created_at_unix: number; updated_at_unix: number }
export type ProjectCandidate = { candidate_id: string; agent_id: string; display_name: string; suggested_project_id: string; session_count: number; state: 'discovered' | 'approved'; updated_at_unix: number; sync_state?: 'unknown' | 'pending' | 'ready' | 'failed'; last_activity_unix?: number }

export async function json<T>(url: string, signal?: AbortSignal): Promise<T> {
  const response = await fetch(url, { credentials: 'same-origin', signal, headers: { Accept: 'application/json' } })
  if (!response.ok) throw await apiError(response)
  return response.json() as Promise<T>
}

export async function fetchExperiments(): Promise<Experiment[]> {
  const value = await json<{ protocol: string; experiments: Experiment[] }>('/api/v1/experiments')
  if (value.protocol !== PROTOCOL_VERSION || !Array.isArray(value.experiments)) throw new Error('Hub returned invalid experiments')
  return value.experiments
}

export async function fetchSessionPage(project?: string, archived: 'false' | 'true' | 'all' = 'false', cursor?: string, signal?: AbortSignal, visibleOnly = false, agent?: string) {
  const params = new URLSearchParams({ archived, limit: '50' }); if (visibleOnly) params.set('visible_only', 'true'); if (agent) params.set('agent', agent); if (project) params.set('project', project); if (cursor) params.set('cursor', cursor)
  const value = await json<{ protocol: string; sessions: CodexSession[]; next_cursor?: string }>(`/api/v1/codex/sessions?${params}`, signal)
  if (value.protocol !== PROTOCOL_VERSION || !Array.isArray(value.sessions)) throw new Error('Hub returned invalid sessions')
  return value
}
export async function fetchSessions(project?: string, archived: 'false' | 'true' | 'all' = 'false') {
  return (await fetchSessionPage(project, archived)).sessions
}

export async function fetchTranscript(sessionId: string, cursor?: string, signal?: AbortSignal): Promise<TranscriptPage> {
  const params = new URLSearchParams({ limit: '20' }); if (cursor) params.set('cursor', cursor)
  const value = await json<TranscriptPage>(`/api/v1/codex/sessions/${encodeURIComponent(sessionId)}/transcript?${params}`, signal)
  if (!Array.isArray(value.turns) || value.session_id !== sessionId) throw new Error('Hub returned invalid transcript')
  return value
}

export async function fetchSchedules(sessionId?: string, signal?: AbortSignal): Promise<CodexSchedule[]> {
  const suffix = sessionId ? `?session=${encodeURIComponent(sessionId)}` : ''
  const value = await json<{ protocol: string; schedules: CodexSchedule[] }>(`/api/v1/codex/schedules${suffix}`, signal)
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
let receiptGeneration = 0
export function clearOperationReceipts() { pendingOperations.clear(); savedReceipts.clear(); receiptGeneration++ }
export type Operation = { command_id?: string; state?: string; detail?: string; status_url?: string; data?: { turn_id?: string; session_id?: string; project_id?: string }; result?: { turn_id?: string; session_id?: string; project_id?: string } }
const codexErrors: Record<string, string> = {
  project_preferences_conflict: '展示设置已在另一台设备更新，已重新读取，请再次选择',
  project_root_revoked: '此根目录的授权已撤销，请重新选择或在服务器授权',
  project_directory_expired: '目录选择已过期，请刷新根目录并重新选择',
  project_directory_changed: '目录已移动或被替换，请重新选择',
  project_directory_unavailable: '目录已消失或无法读取，请刷新后重新选择',
  project_directory_permission: '目录权限已改变，请在服务器检查授权和目录权限',
  project_invalid_name: '名称必须是单段目录名，不能包含斜线、控制字符或相对路径',
  project_name_conflict: '同名目录已存在，请更换名称，或改为接入已有目录',
  project_creation_unconfirmed: '目录创建结果尚未确认，请刷新项目列表并核对原操作',
  project_history_sync_failed: '项目已接入，但历史同步失败，可单独重试同步',
  project_not_approved: '项目尚未接入，请先添加项目',
  project_root_is_container: '根目录仅用于授权范围，请选择它下面的项目目录',
  codex_session_archived: '会话已归档，请先恢复会话再发送',
  codex_archive_busy: '会话或关联子会话仍有活动任务、排队输入或待触发调度，请先处理这些任务',
  codex_archive_unverified: '暂时无法完整核对原生状态。请使用 Codex 0.153.4 或以上版本，并刷新后重试',
  codex_archive_changed: '会话及子会话范围已改变，请重新查看影响范围并确认',
  codex_archive_unapproved: '会话或关联子会话所在项目未授权，或目录已改变，请先接入对应项目',
  codex_archive_unsaved: '会话或关联子会话尚未保存，请先完成首次对话并等待保存',

  codex_session_in_use: '会话正由其他 Codex 客户端占用，请关闭那边的会话连接后再发送',
  model_choice_unavailable: '所选模型或推理强度已不可用，请重新选择',
  codex_handoff_busy: 'Agent 正在读取或执行对话，请结束活动任务后再交接',
  codex_handoff_unsaved: 'Agent 还有尚未落盘的空会话，请先发送消息并等待保存',
  codex_handoff_background: 'Codex 还有后台终端任务，请先在原客户端处理完再交接',
  native_queue_legacy_pending: '旧版排队指令仍未排空，请等待完成或取消后再使用原生队列',
  native_queue_pending_conflict: '此任务有提交正在对账；为避免重复执行，请先等待原操作核对结果',
  codex_ephemeral_queue_unsupported: '当前 Codex 不支持临时任务使用原生队列，请使用普通文字轮次',
  codex_server_request_scope_mismatch: '审批不属于当前任务，请刷新待处理请求',
  codex_server_request_expired: '此请求已过期，未发送批准',
  codex_server_request_resolved: '此请求已处理或撤销，请刷新',
  codex_server_answer_unknown: '回答是否送达尚未确认，请核对原生状态；不会重复发送批准',
  codex_pin_upgrade_required: '当前 Codex 未提供原生置顶状态，请升级服务器 Codex',
  codex_pin_unverified: '尚未确认原生置顶状态，请刷新后核对',
  temporary_session_capacity: '临时任务数量已达上限，请先结束不再使用的临时任务',
  not_temporary_session: '此任务不是当前连接中的临时任务',
  codex_handoff_unverified: '暂时无法确认 Codex 已空闲，连接尚未释放，请稍后重试',
  codex_handoff_unconfirmed: '连接退出尚未确认，请重新核对原生会话状态',
}
export function codexOperationError(detail?: string) { return codexErrors[detail ?? ''] }
export class ApiError extends Error {
  constructor(message: string, public code: string, public status: number) { super(message); this.name = 'ApiError' }
}
async function apiError(response: Response) {
  const value = await response.json().catch(() => ({})) as { error?: string }
  const errors: Record<string, string> = { operation_expired: '这次操作已过期，草稿已保留；核对状态后可修改指令重新提交', operation_failed: '这次操作已失败，草稿已保留；请先核对执行结果', agent_offline: 'Agent 离线，连接恢复后重试', agent_upgrade_required: '当前 Agent 尚不支持此功能，请更新 Agent', agent_save_unconfirmed: '尚未确认 Agent 保存，重试会核对同一次操作', invalid_schedule_time: '时间必须在 60 秒至 365 天之间', session_is_not_running: '当前会话已没有活动对话', visible_turn_changed: '活动对话已改变，请刷新后再操作', idempotency_conflict: '操作身份与之前的请求冲突', invalid_model_choice: '模型或推理强度无效，请重新选择', model_change_requires_queue: '切换模型需要排队下一轮', invalid_session_name: '请输入 1–128 字的明确名称，不能包含路径或控制字符' }
  return new ApiError(errors[value.error ?? ''] ?? codexErrors[value.error ?? ''] ?? (response.status === 401 ? '登录已过期，请重新登录' : `请求失败（HTTP ${response.status}）${value.error ? `：${value.error}` : ''}`), value.error ?? 'request_failed', response.status)
}
export async function mutate(url: string, csrf: string, body?: unknown, method = 'POST', operationId?: string): Promise<Operation> {
  const generation = receiptGeneration
  const encoded = body === undefined ? undefined : JSON.stringify(body)
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(encoded ?? ''))
  if (generation !== receiptGeneration) throw new ApiError('登录状态已改变，请重新提交', 'session_changed', 401)
  const identity = `${method}:${url}:${Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join('')}`
  const key = operationId ?? pendingOperations.get(identity) ?? crypto.randomUUID()
  pendingOperations.set(identity, key)
  if (pendingOperations.size > 256) pendingOperations.delete(pendingOperations.keys().next().value!)
  const response = await fetch(url, { method, credentials: 'same-origin', headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': csrf, 'Idempotency-Key': key }, body: encoded })
  if (!response.ok) {
    if ([400, 401, 403, 404, 409].includes(response.status)) pendingOperations.delete(identity)
    throw await apiError(response)
  }
  const result = response.status === 204 ? {} : await response.json().catch(() => ({})) as Operation
  pendingOperations.delete(identity)
  if (result.command_id && generation === receiptGeneration) { savedReceipts.set(result.command_id, { identity, key }); if (savedReceipts.size > 256) savedReceipts.delete(savedReceipts.keys().next().value!) }
  return result
}
export async function waitForCommand(operation: Operation): Promise<Operation> {
  if (!operation.command_id) return operation
  try {
    const id = operation.command_id
    const initial = queryClient.getQueryData<Operation>(keys.operation(id))
    if (!initial) {
      const status = await json<Operation>(`/api/v1/commands/${encodeURIComponent(id)}`)
      cacheOperation(id, status)
    }
    return await new Promise<Operation>((resolve, reject) => {
      let unsubscribe = () => {}
      const timer = setTimeout(() => { unsubscribe(); reject(new Error('Agent 已接收，操作仍在进行；重试会核对原操作')) }, 30_000)
      const check = () => {
        const status = queryClient.getQueryData<Operation>(keys.operation(id))
        if (!status?.state || !['completed', 'failed', 'expired', 'unknown', 'orphaned'].includes(status.state)) return
        clearTimeout(timer); unsubscribe()
        if (status.state === 'completed') { savedReceipts.delete(id); resolve(status) }
        else {
          if (['failed', 'expired'].includes(status.state)) savedReceipts.delete(id)
          reject(new Error(codexErrors[status.detail ?? ''] ?? `操作结果：${status.state}`))
        }
      }
      const observer = new QueryObserver<Operation>(queryClient, { queryKey: keys.operation(id), queryFn: ({ signal }) => json<Operation>(`/api/v1/commands/${encodeURIComponent(id)}`, signal), staleTime: Infinity })
      unsubscribe = observer.subscribe(check)
      check()
    })
  } catch (error) {
    const receipt = savedReceipts.get(operation.command_id)
    if (receipt) pendingOperations.set(receipt.identity, receipt.key)
    throw error
  }
}

export function createSession(csrf: string, agentId: string, projectId: string, mode: 'inspect' | 'edit', inheritPermissions = false, primaryDirectoryId?: string) {
  return mutate('/api/v1/codex/sessions', csrf, { agent_id: agentId, project_id: projectId, mode, ...(inheritPermissions ? { inherit_permissions: true } : {}), ...(primaryDirectoryId ? { primary_directory_id: primaryDirectoryId } : {}) })
}
export function sendMessage(csrf: string, sessionId: string, prompt: string, delivery: 'queue' | 'steer', turnId?: string, operationId?: string, modelChoice?: ModelChoice) {
  return mutate(`/api/v1/codex/sessions/${encodeURIComponent(sessionId)}/messages`, csrf, { prompt, delivery, ...(delivery === 'steer' ? { turn_id: turnId } : {}), ...(modelChoice ? { model_choice: modelChoice } : {}) }, 'POST', operationId)
}
export function interruptSession(csrf: string, sessionId: string, turnId?: string) {
  return mutate(`/api/v1/codex/sessions/${encodeURIComponent(sessionId)}/interrupt`, csrf, { turn_id: turnId })
}
export function handoffSession(csrf: string, sessionId: string) {
  return mutate(`/api/v1/codex/sessions/${encodeURIComponent(sessionId)}/handoff`, csrf, {})
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

export type ProjectPreference = { agent_id: string; project_id: string; display_name: string | null; hidden: boolean; pinned: boolean; manual_order?: number }
export type ProjectPreferences = { revision: number; projects: ProjectPreference[] }
export async function fetchProjectPreferences() { const value = await json<ProjectPreferences>('/api/v1/project-preferences'); if (!Number.isSafeInteger(value.revision) || !Array.isArray(value.projects)) throw new Error('项目展示设置读取失败，请更新 Hub 并重试'); return value }
export async function saveProjectPreferences(csrf: string, revision: number, projects: ProjectPreference[]) { return await mutate('/api/v1/project-preferences', csrf, { revision, projects }, 'PUT') as unknown as ProjectPreferences }
export type ProjectDirectory = { directory_id: string; name: string }
export type DirectoryPage = { directory: ProjectDirectory; parent_id: string | null; entries: ProjectDirectory[]; next_cursor: string | null }
export const fetchProjectRoots = (agent: string, signal?: AbortSignal) => json<{ roots: ProjectDirectory[] }>(`/api/v1/agents/${encodeURIComponent(agent)}/project-roots`, signal)
export const fetchProjectDirectories = (agent: string, directory: string, cursor?: string, signal?: AbortSignal) => json<DirectoryPage>(`/api/v1/agents/${encodeURIComponent(agent)}/project-directories?${new URLSearchParams({ directory_id: directory, ...(cursor ? { cursor } : {}) })}`, signal)
export type ProjectMember = { directory_id: string; name: string; is_primary: boolean; can_create_worktree: boolean }
export const fetchProjectInfo = (agent: string, project: string, signal?: AbortSignal) => json<{ can_create_worktree: boolean; directories?: ProjectMember[] }>(`/api/v1/agents/${encodeURIComponent(agent)}/projects/${encodeURIComponent(project)}/info`, signal)
export const addProject = (csrf: string, agent: string, directory: string, name?: string, projectId?: string) => mutate('/api/v1/projects', csrf, { agent_id: agent, directory_id: directory, ...(projectId ? { kind: 'attach_to', project_id: projectId } : name === undefined ? { kind: 'attach' } : { kind: 'create', name }) })
export const syncProject = (csrf: string, agent: string, project: string) => mutate(`/api/v1/agents/${encodeURIComponent(agent)}/projects/${encodeURIComponent(project)}/sync`, csrf)
export type ArchivePreview = { session_ids: string[]; fingerprint: string; can_archive: boolean; reason: string | null }
export const fetchArchivePreview = (session: string, signal?: AbortSignal) => json<ArchivePreview>(`/api/v1/codex/sessions/${encodeURIComponent(session)}/archive-preview`, signal)
export const archiveSession = (csrf: string, session: string, fingerprint: string) => mutate(`/api/v1/codex/sessions/${encodeURIComponent(session)}/archive`, csrf, { fingerprint })
export const unarchiveSession = (csrf: string, session: string) => mutate(`/api/v1/codex/sessions/${encodeURIComponent(session)}/unarchive`, csrf)
