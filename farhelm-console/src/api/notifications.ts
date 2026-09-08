import { json, mutate } from './features'
export type Notice = { id: number; event_id: string; agent_id: string; category: string; state: string; target_id: string; title: string; message: string; created_at_unix: number; read_at_unix?: number }
export type NoticePage = { notifications: Notice[]; next_cursor?: number; unread_count: number; latest_id: number; preferences?: { experiments: boolean; codex: boolean } }
export type Device = { id: string; name: string; experiments: boolean; codex: boolean }
export type Delivery = { device_id: string; state: string; attempts: number; last_error?: string }
export function notices(params = new URLSearchParams(), signal?: AbortSignal) { return json<NoticePage>(`/api/v1/notifications?${params}`, signal) }
export type NoticeDetail = { notification: Notice; deliveries: Delivery[] }
export function noticeDetail(id: number, signal?: AbortSignal) { return json<NoticeDetail>(`/api/v1/notifications/${id}`, signal) }
export function markRead(csrf: string, id: number) { return mutate(`/api/v1/notifications/${id}/read`, csrf) }
export function markAllRead(csrf: string, through: number) { return mutate('/api/v1/notifications/read-all', csrf, { through_id: through }) }
export function devices() { return json<{ devices: Device[] }>('/api/v1/push/devices') }
export function updateDevice(csrf: string, device: Device) { return mutate(`/api/v1/push/devices/${device.id}`, csrf, device) }
export function removeDevice(csrf: string, id: string) { return mutate(`/api/v1/push/devices/${id}`, csrf, undefined, 'DELETE') }
export function testDevice(csrf: string, id: string) { return mutate(`/api/v1/push/devices/${id}/test`, csrf) }
export const stateLabel = (state: string) => ({ succeeded: '已完成', failed: '失败', unknown: '结果未知', watching: '观察中', cancelled: '已取消', pending: '待投递', accepted: '推送服务已接受', expired: '已过期' })[state] ?? state
