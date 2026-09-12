import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Alert, Button, Popover, Select, Space } from 'antd'
import { fetchNativeControl, operateNative, waitForCommand } from '../../api/features'
import { keys, queryClient } from '../../api/cache'
import { errorText } from './presentation'

export function NativeSettings({ csrf, session, disabled }: { csrf: string; session: string; disabled: boolean }) {
  const [open, setOpen] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string>()
  const permissions = useQuery({ queryKey: ['codex', 'native', session, 'permissions'], enabled: open, queryFn: ({ signal }) => fetchNativeControl<{ data: { id: string; description?: string; allowed: boolean }[] }>(session, 'permissions', signal) })
  const settings = useQuery({ queryKey: ['codex', 'native', session, 'settings'], enabled: open, queryFn: ({ signal }) => fetchNativeControl<{ modes: { name: string; mode?: string; model?: string; reasoning_effort?: string }[] }>(session, 'settings', signal) })
  const apply = async (value: { permissions?: string; model?: string; collaboration_mode?: string; reasoning_effort?: string }) => {
    setBusy(true); setError(undefined)
    try { await waitForCommand(await operateNative(csrf, session, { operation: 'thread_settings', settings: value })); void queryClient.invalidateQueries({ queryKey: keys.history(session) }); setOpen(false) }
    catch (error) { setError(errorText(error)) }
    finally { setBusy(false) }
  }
  return <Popover placement="top" trigger="click" open={open} onOpenChange={setOpen} content={<Space direction="vertical" className="native-control-stack">
    <span>应用到后续轮次</span>
    <Select aria-label="原生协作模式" placeholder="协作模式" disabled={busy} options={(settings.data?.data.modes ?? []).filter(mode => mode.mode && mode.model).map(mode => ({ value: mode.mode!, label: mode.name }))} onChange={value => { const mode = settings.data?.data.modes.find(mode => mode.mode === value); if (mode?.model) void apply({ collaboration_mode: value, model: mode.model, ...(mode.reasoning_effort ? { reasoning_effort: mode.reasoning_effort } : {}) }) }} />
    <Select aria-label="原生权限档位" placeholder="服务器允许的权限档位" disabled={busy} options={(permissions.data?.data.data ?? []).filter(profile => profile.allowed).map(profile => ({ value: profile.id, label: profile.description ?? profile.id }))} onChange={permissions => void apply({ permissions })} />
    {(error || permissions.error || settings.error) && <Alert type="warning" title={error ?? errorText(permissions.error ?? settings.error)} />}
  </Space>}><Button disabled={disabled} loading={busy}>模式与权限</Button></Popover>
}
