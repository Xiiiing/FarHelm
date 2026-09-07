import { Alert, Button, Drawer, Form, Input, List, Modal, Radio, Segmented, Select, Tag } from 'antd'
import { useEffect, useRef, useState } from 'react'
import { cancelSchedule, createSchedule, createSession, fetchExperiments, fetchSchedules, waitForCommand, type CodexSchedule, type CodexSession, type Experiment, type ProjectCandidate } from '../../api/features'
import { subscribeEvents } from '../../api/events'
import { errorText, sessionName, stateNames } from './presentation'

export function CreateDialog({ csrf, projects, onClose, onCreated }: { csrf: string; projects: ProjectCandidate[]; onClose: () => void; onCreated: (sessionId?: string) => void }) {
  const [error, setError] = useState<string>(); const [busy, setBusy] = useState(false)
  const active = useRef(true)
  useEffect(() => { active.current = true; return () => { active.current = false } }, [])
  return <Modal title="创建 Codex 会话" open onCancel={onClose} footer={null}><Form layout="vertical" onFinish={async (values: { project: string; mode: 'inspect' | 'edit' }) => {
    const project = projects.find((p) => p.candidate_id === values.project && p.state === 'approved'); if (!project || busy) return
    setBusy(true); setError(undefined)
    try { const result = await waitForCommand(await createSession(csrf, project.agent_id, project.suggested_project_id, values.mode)); if (active.current) { onCreated(result.data?.session_id ?? result.result?.session_id); onClose() } } catch (e) { if (active.current) setError(errorText(e)) } finally { if (active.current) setBusy(false) }
  }}>{error && <Alert type="error" showIcon title={error} />}<Form.Item label="项目" name="project" rules={[{ required: true }]}><Select options={projects.filter((p) => p.state === 'approved').map((p) => ({ value: p.candidate_id, label: `${p.display_name} · ${p.agent_id}` }))} /></Form.Item><Form.Item label="模式" name="mode" initialValue="inspect"><Radio.Group><Radio value="inspect">只读</Radio><Radio value="edit">编辑（隔离工作区）</Radio></Radio.Group></Form.Item><Button type="primary" htmlType="submit" loading={busy} block>创建</Button></Form></Modal>
}

export function ScheduleDialog({ csrf, target, prompt, onClose }: { csrf: string; target: CodexSession; prompt: string; onClose: () => void }) {
  const [error, setError] = useState<string>(); const [busy, setBusy] = useState(false); const [experiments, setExperiments] = useState<Experiment[]>([])
  useEffect(() => { let active = true; void fetchExperiments().then((rows) => { if (active) setExperiments(rows) }).catch((e: unknown) => { if (active) setError(errorText(e)) }); return () => { active = false } }, [])
  return <Modal title={`定时发送 · ${sessionName(target)}`} open onCancel={onClose} footer={null}><Form layout="vertical" initialValues={{ delivery: 'at_time', prompt }} onFinish={async (values: { delivery: string; prompt: string; runAt?: string; watchId?: string }) => {
    if (busy) return; setBusy(true); setError(undefined)
    const trigger = values.delivery === 'at_time' ? { type: 'at_time' as const, run_at_unix: Math.floor(new Date(values.runAt!).getTime() / 1000) } : { type: 'experiment_succeeded' as const, watch_id: values.watchId! }
    try { await waitForCommand(await createSchedule(csrf, target.session_id, values.prompt, trigger)); onClose() } catch (e) { setError(errorText(e)) } finally { setBusy(false) }
  }}>{error && <Alert type="error" showIcon title={error} />}<Form.Item name="delivery" label="触发方式"><Segmented block options={[{ label: '指定时间', value: 'at_time' }, { label: '训练成功后', value: 'experiment_succeeded' }]} /></Form.Item><Form.Item noStyle shouldUpdate>{({ getFieldValue }) => getFieldValue('delivery') === 'at_time' ? <Form.Item name="runAt" label="发送时间（本地时区）" rules={[{ required: true }]}><Input type="datetime-local" /></Form.Item> : <Form.Item name="watchId" label="训练任务" rules={[{ required: true }]}><Select options={experiments.filter((e) => e.state === 'watching' && e.agent_id === target.agent_id && e.project_id === target.project_id).map((e) => ({ value: e.watch_id, label: e.name }))} /></Form.Item>}</Form.Item><Form.Item name="prompt" label="指令" rules={[{ required: true }, { max: 32768 }]}><Input.TextArea autoSize={{ minRows: 4, maxRows: 10 }} /></Form.Item><Button type="primary" htmlType="submit" loading={busy} block>创建定时任务</Button></Form></Modal>
}

export function SchedulesDrawer({ csrf, target, onClose }: { csrf: string; target: CodexSession; onClose: () => void }) {
  const [rows, setRows] = useState<CodexSchedule[]>([]); const [error, setError] = useState<string>()
  useEffect(() => {
    let active = true
    const load = () => { void fetchSchedules(target.session_id).then((rows) => { if (active) setRows(rows) }).catch((e: unknown) => { if (active) setError(errorText(e)) }) }
    load(); const off = subscribeEvents(['codex.schedule.updated'], load)
    return () => { active = false; off() }
  }, [target.session_id])
  return <Drawer title={`定时任务 · ${sessionName(target)}`} open onClose={onClose}>{error && <Alert title={error} type="error" showIcon />}<List locale={{ emptyText: '当前会话没有定时任务' }} dataSource={rows} renderItem={(item) => <List.Item actions={['pending', 'queued'].includes(item.state) ? [<Button key="cancel" danger onClick={() => void cancelSchedule(csrf, item.schedule_id).then(waitForCommand).then(() => fetchSchedules(target.session_id)).then(setRows).catch((e: unknown) => setError(errorText(e)))}>取消</Button>] : undefined}><List.Item.Meta title={item.trigger.type === 'at_time' ? new Date(item.trigger.run_at_unix * 1000).toLocaleString() : '训练成功后'} description={<Tag>{stateNames[item.state] ?? item.state}</Tag>} /></List.Item>} /></Drawer>
}
