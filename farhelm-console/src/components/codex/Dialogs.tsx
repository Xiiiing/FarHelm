import { Alert, Button, Drawer, Form, Input, List, Modal, Checkbox, Popconfirm, Segmented, Select, Tag } from 'antd'
import { useEffect, useRef, useState } from 'react'
import { cancelSchedule, createSchedule, createSession, fetchExperiments, fetchSchedules, fetchProjectInfo, waitForCommand, type CodexSession, type ProjectCandidate } from '../../api/features'
import { useMutation, useQuery } from '@tanstack/react-query'
import { queryClient } from '../../api/cache'
import { errorText, sessionName, stateNames } from './presentation'
import { useAgents } from '../../hooks/useAgents'

export function CreateDialog({ csrf, projects, initialProject, onClose, onCreated }: { csrf: string; projects: ProjectCandidate[]; initialProject?: ProjectCandidate; onClose: () => void; onCreated: (sessionId?: string) => void }) {
  const [error, setError] = useState<string>(); const [busy, setBusy] = useState(false)
  const [form] = Form.useForm<{ project: string; isolated: boolean }>()
  const selectedKey = Form.useWatch('project', form)
  const projectKey = (project: ProjectCandidate) => JSON.stringify([project.agent_id, project.candidate_id])
  const project = projects.find((p) => projectKey(p) === selectedKey && p.state === 'approved')
  const { agents } = useAgents()
  const supportsNative = agents.state === 'ready' && agents.data.agents.find((a) => a.agent_id === project?.agent_id)?.capabilities?.includes('codex.session_context')
  const info = useQuery({ queryKey: ['project-info', project?.agent_id, project?.suggested_project_id], enabled: !!project, queryFn: ({ signal }) => fetchProjectInfo(project!.agent_id, project!.suggested_project_id, signal), staleTime: 0 })
  const active = useRef(true)
  useEffect(() => { active.current = true; return () => { active.current = false } }, [])
  return <Modal title="创建 Codex 会话" open onCancel={onClose} footer={null}><Form className="create-session-form" form={form} layout="vertical" initialValues={{ isolated: false, project: initialProject ? projectKey(initialProject) : undefined }} onFinish={async (values) => {
    if (!project || busy || !supportsNative) return
    setBusy(true); setError(undefined)
    try {
      const result = await waitForCommand(await createSession(csrf, project.agent_id, project.suggested_project_id, values.isolated && info.data?.can_create_worktree ? 'edit' : 'inspect', true))
      if (active.current) { onCreated(result.data?.session_id ?? result.result?.session_id); onClose() }
    } catch (e) { if (active.current) setError(errorText(e)) } finally { if (active.current) setBusy(false) }
  }}>
    {error && <Alert type="error" showIcon title={error} />}
    <Form.Item label="项目" name="project" rules={[{ required: true }]}><Select options={projects.filter((p) => p.state === 'approved').map((p) => ({ value: projectKey(p), label: `${p.display_name} · ${p.agent_id}` }))} /></Form.Item>
    <p className="settings-detail-note">模型和权限跟随服务器上的 Codex 项目配置。</p>
    <Form.Item name="isolated" valuePropName="checked" extra="单独的 Git 工作区，适合希望与当前项目修改分开的任务。"><Checkbox disabled={!info.data?.can_create_worktree}>使用隔离工作区</Checkbox></Form.Item>
    {info.error && <Alert type="warning" title="项目状态读取失败" description={errorText(info.error)} action={<Button onClick={() => void info.refetch()}>重试</Button>} />}
    {project && !info.error && !info.isPending && !info.data?.can_create_worktree && <p className="settings-detail-note">当前项目没有可用的 Git 提交，隔离工作区不可用。</p>}
    {project && !supportsNative && <Alert showIcon type="warning" title="请升级此 Agent 后使用原生会话权限" />}
    <Button type="primary" htmlType="submit" loading={busy} disabled={!supportsNative} block>创建会话</Button>
  </Form></Modal>
}

const scheduleTimeRule = { validator: (_: unknown, value?: string) => {
  if (!value) return Promise.resolve()
  const seconds = (new Date(value).getTime() - Date.now()) / 1000
  return Number.isFinite(seconds) && seconds >= 60 && seconds <= 365 * 86400 ? Promise.resolve() : Promise.reject(new Error('发送时间必须在 60 秒至 365 天之后'))
} }

export function ScheduleDialog({ csrf, target, prompt, onClose }: { csrf: string; target: CodexSession; prompt: string; onClose: () => void }) {
  const [error, setError] = useState<string>(); const [busy, setBusy] = useState(false)
  const active = useRef(true)
  useEffect(() => { active.current = true; return () => { active.current = false } }, [])
  const experiments = useQuery({ queryKey: ['experiments'], queryFn: fetchExperiments })
  return <Modal title={`定时发送 · ${sessionName(target)}`} open onCancel={onClose} footer={null}><Form layout="vertical" initialValues={{ delivery: 'at_time', prompt }} onFinish={async (values: { delivery: string; prompt: string; runAt?: string; watchId?: string }) => {
    if (busy) return; setBusy(true); setError(undefined)
    const trigger = values.delivery === 'at_time' ? { type: 'at_time' as const, run_at_unix: Math.floor(new Date(values.runAt!).getTime() / 1000) } : { type: 'experiment_succeeded' as const, watch_id: values.watchId! }
    try {
      await waitForCommand(await createSchedule(csrf, target.session_id, values.prompt, trigger))
      void queryClient.invalidateQueries({ queryKey: ['codex', 'schedules', target.session_id] })
      if (active.current) onClose()
    } catch (e) { if (active.current) setError(errorText(e)) } finally { if (active.current) setBusy(false) }
  }}>{error && <Alert type="error" showIcon title={error} />}<Form.Item name="delivery" label="触发方式"><Segmented block options={[{ label: '指定时间', value: 'at_time' }, { label: '训练成功后', value: 'experiment_succeeded' }]} /></Form.Item><Form.Item noStyle shouldUpdate>{({ getFieldValue }) => getFieldValue('delivery') === 'at_time' ? <Form.Item name="runAt" label="发送时间（本地时区）" rules={[{ required: true }, scheduleTimeRule]}><Input type="datetime-local" /></Form.Item> : <>{experiments.error && <Alert type="warning" title="无法读取训练任务" action={<Button onClick={() => void experiments.refetch()}>重试</Button>} />}<Form.Item name="watchId" label="训练任务" rules={[{ required: true }]}><Select loading={experiments.isFetching} options={(experiments.data ?? []).filter((e) => e.state === 'watching' && e.agent_id === target.agent_id && e.project_id === target.project_id).map((e) => ({ value: e.watch_id, label: e.name }))} /></Form.Item></>}</Form.Item><p className="settings-detail-note">执行时沿用这条会话的 Codex 模型与权限。</p><Form.Item name="prompt" label="指令" rules={[{ required: true }, { max: 32768 }]}><Input.TextArea autoSize={{ minRows: 4, maxRows: 10 }} /></Form.Item><Button type="primary" htmlType="submit" loading={busy} block>创建定时任务</Button></Form></Modal>
}

export function SchedulesDrawer({ csrf, target, onClose }: { csrf: string; target: CodexSession; onClose: () => void }) {
  const rows = useQuery({ queryKey: ['codex', 'schedules', target.session_id], queryFn: ({ signal }) => fetchSchedules(target.session_id, signal), refetchOnMount: 'always' })
  const cancel = useMutation({ mutationFn: (id: string) => cancelSchedule(csrf, id).then(waitForCommand), onSuccess: () => queryClient.invalidateQueries({ queryKey: ['codex', 'schedules', target.session_id] }) })
  return <Drawer title={`定时任务 · ${sessionName(target)}`} open onClose={onClose}>
    {(cancel.error || rows.error) && <Alert title={errorText(cancel.error ?? rows.error)} type="error" showIcon />}
    <Button loading={rows.isFetching} onClick={() => void rows.refetch()}>刷新任务</Button>
    <List locale={{ emptyText: rows.error ? '定时任务暂时不可用，请重试' : '当前会话没有定时任务' }} loading={rows.isPending} dataSource={rows.data ?? []} rowKey="schedule_id" renderItem={item => <List.Item actions={['pending', 'queued'].includes(item.state) ? [
      <Popconfirm key="cancel" title="取消这项定时任务？" description={`${sessionName(target)} · ${item.schedule_id.slice(0, 8)}，仅取消尚未开始的指令。`} okText="确认取消" cancelText="保留任务" onConfirm={() => cancel.mutateAsync(item.schedule_id)} disabled={cancel.isPending}>
        <Button danger disabled={cancel.isPending} loading={cancel.isPending && cancel.variables === item.schedule_id}>取消</Button>
      </Popconfirm>,
    ] : undefined}><List.Item.Meta title={item.trigger.type === 'at_time' ? new Date(item.trigger.run_at_unix * 1000).toLocaleString() : '训练成功后'} description={<Tag>{stateNames[item.state] ?? item.state}</Tag>} /></List.Item>} />
  </Drawer>
}
