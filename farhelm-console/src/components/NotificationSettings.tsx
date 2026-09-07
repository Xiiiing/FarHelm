import { Alert, Button, Card, Space, Switch, Typography } from 'antd'
import { useEffect, useState } from 'react'
import { json, mutate } from '../api/features'
import { subscribeEvents } from '../api/events'

type Preferences = { experiments: boolean; codex: boolean }
export function NotificationSettings({ csrf }: { csrf: string }) {
  const [preferences, setPreferences] = useState<Preferences>()
  const [error, setError] = useState<string>()
  const [busy, setBusy] = useState(false)
  useEffect(() => {
    let active = true
    const load = () => { void json<Preferences>('/api/v1/notifications/preferences').then((value) => { if (active) setPreferences(value) }).catch((e: unknown) => { if (active) setError(String(e)) }) }
    load(); const off = subscribeEvents(['notification.changed', 'open'], load)
    return () => { active = false; off() }
  }, [])
  const save = async (value: Preferences) => {
    setBusy(true)
    try { await mutate('/api/v1/notifications/preferences', csrf, value); setPreferences(value); setError(undefined) }
    catch (e) { setError(e instanceof Error ? e.message : '通知偏好保存失败') }
    finally { setBusy(false) }
  }
  const test = async () => {
    setBusy(true)
    try { await mutate('/api/v1/notifications/test', csrf); setError(undefined) }
    catch (e) { setError(e instanceof Error ? e.message : '测试通知失败') }
    finally { setBusy(false) }
  }
  return <Card title="页面通知">
    <Space orientation="vertical" size={16}>
      <Typography.Paragraph>保持 FarHelm 页面打开，可接收训练和 Codex 的完成提醒。通知记录与已读状态随账户同步。</Typography.Paragraph>
      {error && <Alert type="error" title="通知设置失败" description={error} />}
      <Space><Switch aria-label="实验页面提醒" disabled={busy || !preferences} checked={preferences?.experiments ?? true} onChange={(experiments) => { if (preferences) void save({ ...preferences, experiments }) }} />实验结果提醒</Space>
      <Space><Switch aria-label="Codex 页面提醒" disabled={busy || !preferences} checked={preferences?.codex ?? true} onChange={(codex) => { if (preferences) void save({ ...preferences, codex }) }} />Codex 每轮结果提醒</Space>
      <Button loading={busy} onClick={() => void test()}>发送页面测试通知</Button>
    </Space>
  </Card>
}
