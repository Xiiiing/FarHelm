import { ArrowLeftOutlined, FolderOpenOutlined, PlusOutlined, PushpinOutlined } from '@ant-design/icons'
import { Alert, Button, Checkbox, Drawer, Empty, Form, Input, List, Modal, Segmented, Select, Space, Tabs, Tag, Typography } from 'antd'
import { useInfiniteQuery, useMutation, useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import type { AgentSummary } from '../../api/agents'
import { addProject, fetchProjectDirectories, fetchProjectRoots, importProjects, syncProject, waitForCommand, type ProjectCandidate } from '../../api/features'
import { queryClient } from '../../api/cache'
import { projectKey, useProjects } from '../../hooks/useProjects'
import { errorText } from './presentation'

export function ProjectsDrawer({ csrf, agents, open, onClose, onNew, initialTab = 'manage' }: { csrf: string; agents: AgentSummary[]; open: boolean; onClose: () => void; onNew?: (project: ProjectCandidate) => void; initialTab?: string }) {
  const model = useProjects(csrf)
  const [tab, setTab] = useState(initialTab)
  const [mode, setMode] = useState('discovered')
  const [agentId, setAgentId] = useState<string>()
  const agent = agents.find((a) => a.agent_id === (agentId ?? agents[0]?.agent_id))
  const [directory, setDirectory] = useState<string>()
  const [name, setName] = useState('')
  const [selected, setSelected] = useState<string[]>([])
  const [renaming, setRenaming] = useState<ProjectCandidate>()
  const [renameForm] = Form.useForm<{ name: string }>()
  const [nextSession, setNextSession] = useState<ProjectCandidate>()
  const [success, setSuccess] = useState<string>()
  const canBrowse = Boolean(agent?.online && agent.capabilities?.includes('project.management'))
  const roots = useQuery({ queryKey: ['project-roots', agent?.agent_id], enabled: open && tab === 'add' && mode !== 'discovered' && canBrowse, queryFn: ({ signal }) => fetchProjectRoots(agent!.agent_id, signal), staleTime: 0 })
  const directories = useInfiniteQuery({ queryKey: ['project-directories', agent?.agent_id, directory], enabled: open && tab === 'add' && mode !== 'discovered' && canBrowse && !!directory, initialPageParam: undefined as string | undefined, queryFn: ({ pageParam, signal }) => fetchProjectDirectories(agent!.agent_id, directory!, pageParam, signal), getNextPageParam: (page) => page.next_cursor ?? undefined, staleTime: 0 })
  const refresh = async () => { await model.catalog.refetch(); await queryClient.invalidateQueries({ queryKey: ['codex', 'sessions'] }) }
  const operation = useMutation({ mutationFn: async (work: () => Promise<void>) => { setSuccess(undefined); await work() }, onSuccess: refresh })
  const busy = operation.isPending || model.save.isPending
  const pending = model.projects.filter((p) => p.state === 'discovered')
  const batchImport = (items: ProjectCandidate[]) => operation.mutate(async () => {
    const grouped = new Map<string, string[]>()
    for (const p of items) grouped.set(p.agent_id, [...grouped.get(p.agent_id) ?? [], p.candidate_id])
    for (const [agent, ids] of grouped) for (let i = 0; i < ids.length; i += 100) {
      await waitForCommand(await importProjects(csrf, agent, ids.slice(i, i + 100)))
      await model.catalog.refetch()
    }
    setSelected([]); setSuccess('项目已接入，历史会话正在同步。')
  })
  const page = directories.data?.pages[0]
  const errors = model.catalog.error ?? model.preferences.error ?? model.save.error ?? operation.error
  const newSession = (p: ProjectCandidate) => { setNextSession(p); onClose() }
  const manage = <Space orientation="vertical" className="project-manager-stack" size="middle">
    <Typography.Paragraph type="secondary">选择要展示的项目。隐藏项目后，当前会话和未发送草稿会保留；这里始终列出全部项目。</Typography.Paragraph>
    <Space wrap><Button disabled={busy || !model.preferences.data || !model.approved.length} onClick={() => model.save.mutate(model.approved.map((p) => ({ ...model.preference(p), hidden: false })))}>全选展示</Button><Button disabled={busy || !model.preferences.data || !model.approved.length} onClick={() => model.save.mutate(model.approved.map((p) => ({ ...model.preference(p), hidden: true })))}>取消全选</Button><Button icon={<PlusOutlined />} onClick={() => setTab('add')}>添加项目</Button></Space>
    <List loading={model.catalog.isPending} locale={{ emptyText: '尚未接入项目，请添加项目' }} dataSource={model.approved} rowKey={projectKey} renderItem={(p) => {
      const pref = model.preference(p); const online = agents.find((a) => a.agent_id === p.agent_id)?.online
      return <List.Item className="project-manager-row"><div className="project-manager-title"><Checkbox checked={!pref.hidden} disabled={busy || !model.preferences.data} onChange={(e) => model.save.mutate([{ ...pref, hidden: !e.target.checked }])} aria-label={`展示 ${p.display_name} · ${p.agent_id}`} /><div><strong>{p.display_name}</strong><div className="settings-detail-note">{p.agent_id} · {p.suggested_project_id}</div></div>{pref.pinned && <PushpinOutlined aria-label="已置顶" />}</div><Space wrap>
        {!online ? <Tag>服务器离线</Tag> : p.sync_state === 'failed' ? <Tag color="warning">已接入，历史同步失败</Tag> : p.sync_state === 'pending' ? <Tag>历史同步中</Tag> : <Tag color="success">已接入</Tag>}{pref.hidden && <Tag>已隐藏</Tag>}
        <Button disabled={busy || !model.preferences.data} onClick={() => model.save.mutate([{ ...pref, pinned: !pref.pinned }])}>{pref.pinned ? '取消置顶' : '置顶'}</Button>
        <Button disabled={busy || !model.preferences.data} onClick={() => { renameForm.setFieldsValue({ name: p.display_name }); setRenaming(p) }}>修改显示名称</Button>
        {onNew && <Button disabled={!online} onClick={() => newSession(p)}>新建会话</Button>}
        {p.sync_state === 'failed' && <Button disabled={busy || !online} onClick={() => operation.mutate(async () => { await waitForCommand(await syncProject(csrf, p.agent_id, p.suggested_project_id)); setSuccess('历史同步已完成') })}>重试同步</Button>}
      </Space></List.Item>
    }} />
    {pending.length > 0 && <Button onClick={() => { setTab('add'); setMode('discovered') }}>{pending.length} 个项目待导入</Button>}
  </Space>
  const discovered = <Space orientation="vertical" className="project-manager-stack" size="middle">
    <Space wrap><Button disabled={busy || !selected.length} onClick={() => batchImport(pending.filter((p) => selected.includes(p.candidate_id)))}>导入所选（{selected.length}）</Button><Button disabled={busy || !pending.length} onClick={() => batchImport(pending)}>全部导入（{pending.length}）</Button></Space>
    <List loading={model.catalog.isPending} dataSource={pending} rowKey="candidate_id" locale={{ emptyText: '没有待导入项目，也可以接入已有目录或创建空项目。' }} renderItem={(p) => <List.Item className="project-manager-row"><Space><Checkbox aria-label={`选择导入 ${p.display_name} · ${p.agent_id}`} disabled={busy} checked={selected.includes(p.candidate_id)} onChange={(e) => setSelected((old) => e.target.checked ? [...old, p.candidate_id] : old.filter((id) => id !== p.candidate_id))} /><div>{p.display_name}<div className="settings-detail-note">{p.agent_id} · {p.session_count} 个会话</div></div></Space><Button loading={operation.isPending} disabled={busy || !agents.find((a) => a.agent_id === p.agent_id)?.online} onClick={() => batchImport([p])}>导入此项目</Button></List.Item>} />
  </Space>
  const browse = <Space orientation="vertical" className="project-manager-stack" size="middle">
    <label htmlFor="project-agent">服务器</label><Select id="project-agent" aria-label="项目服务器" value={agent?.agent_id} placeholder="请先添加服务器" options={agents.map((a) => ({ value: a.agent_id, label: `${a.hostname} · ${a.agent_id}` }))} onChange={(v) => { setAgentId(v); setDirectory(undefined); operation.reset() }} />
    {agent && !agent.online && <Alert type="warning" title="服务器离线，连接恢复后可以继续选择目录" />}
    {agent?.online && !canBrowse && <Alert type="warning" title="请升级此 Agent 后使用目录管理" />}
    {canBrowse && <>
      {(roots.error || directories.error) && <Alert type="warning" title={errorText(roots.error ?? directories.error)} action={<Button onClick={() => { setDirectory(undefined); void roots.refetch() }}>重新选择</Button>} />}
      {roots.data?.roots.length === 0 && <Alert type="info" title="尚未授权项目根目录" description={<><p>先在这台服务器授权项目的父目录，再回到这里刷新。</p><Typography.Text code>farhelm-agent project roots add /项目父目录 --name 项目目录</Typography.Text><p>授权后可浏览和创建子目录，具体项目仍需在这里接入。</p><Button onClick={() => void roots.refetch()}>刷新授权目录</Button></>} />}
      {!!roots.data?.roots.length && <><Select aria-label="授权根目录" placeholder="选择授权根目录" value={directory?.startsWith('rtd_') ? directory : undefined} loading={roots.isFetching} options={roots.data.roots.map((r) => ({ value: r.directory_id, label: r.name }))} onChange={(v) => { setDirectory(v); operation.reset() }} />
        {page && <><Space wrap>{page.parent_id && <Button icon={<ArrowLeftOutlined />} onClick={() => setDirectory(page.parent_id!)}>上一级</Button>}<Typography.Text strong>当前目录：{page.directory.name}</Typography.Text></Space>
          <List loading={directories.isFetching} locale={{ emptyText: directories.error ? '读取失败，请重新选择' : '此目录下没有可选子目录' }} dataSource={directories.data?.pages.flatMap((p) => p.entries) ?? []} rowKey="directory_id" renderItem={(entry) => <List.Item><Button block type="text" aria-label={entry.name} icon={<FolderOpenOutlined aria-hidden />} onClick={() => { setDirectory(entry.directory_id); operation.reset() }}>{entry.name}</Button></List.Item>} />
          {directories.hasNextPage && <Button onClick={() => void directories.fetchNextPage()}>更多目录</Button>}
          {mode === 'create' && <><label htmlFor="new-project-name">新项目名称</label><Input id="new-project-name" maxLength={128} value={name} onChange={(e) => setName(e.target.value)} placeholder="在当前目录内创建空项目" /></>}
          <Button type="primary" loading={operation.isPending} disabled={busy || !!directories.error || (mode === 'attach' ? directory?.startsWith('rtd_') : !name || name.trim() !== name || (/[/\\]/.test(name) || [...name].some((c) => c.charCodeAt(0) < 32 || c.charCodeAt(0) === 127)) || ['.', '..'].includes(name) || name.startsWith('.farhelm-'))} onClick={() => operation.mutate(async () => {
            await waitForCommand(await addProject(csrf, agent!.agent_id, directory!, mode === 'create' ? name : undefined))
            setName(''); setSuccess('项目已接入，可在“展示管理”中新建会话。历史同步会在后台继续。'); setTab('manage')
          })}>{mode === 'create' ? '创建并接入空项目' : '接入当前目录'}</Button>
          {mode === 'attach' && directory?.startsWith('rtd_') && <Typography.Text type="secondary">请选择根目录下面的项目目录。</Typography.Text>}
        </>}
      </>}
    </>}
  </Space>
  return <><Drawer className="project-management-drawer" title="项目管理" aria-label="项目管理" open={open} onClose={onClose} size={Math.min(560, window.innerWidth)} afterOpenChange={(visible) => { if (!visible && nextSession) { onNew?.(nextSession); setNextSession(undefined) } else if (!visible && window.innerWidth < 768) { document.querySelector<HTMLButtonElement>('button[aria-label="打开会话列表"]')?.focus() } }}>
    {errors && <Alert type="warning" showIcon title={errorText(errors)} action={<Button onClick={() => { void model.catalog.refetch(); void model.preferences.refetch(); operation.reset(); model.save.reset() }}>重新读取</Button>} />}
    {success && <Alert type="success" showIcon title={success} />}
    <Tabs activeKey={tab} onChange={setTab} items={[{ key: 'manage', label: '展示管理', children: manage }, { key: 'add', label: '添加项目', children: <Space orientation="vertical" className="project-manager-stack" size="middle"><Segmented block value={mode} onChange={(v) => { setMode(v); operation.reset() }} options={[{ value: 'discovered', label: '导入已发现项目' }, { value: 'attach', label: '接入已有目录' }, { value: 'create', label: '创建空项目' }]} />{mode === 'discovered' ? discovered : browse}</Space> }]} />
    {!agents.length && <Empty description="请先在服务器页面添加 Agent" />}
  </Drawer><Modal title="修改项目显示名称" open={!!renaming} onCancel={() => setRenaming(undefined)} onOk={() => renameForm.submit()} confirmLoading={model.save.isPending}><Form form={renameForm} layout="vertical" onFinish={(v) => { if (renaming) model.save.mutate([{ ...model.preference(renaming), display_name: v.name.trim() || null }], { onSuccess: () => setRenaming(undefined) }) }}><Form.Item name="name" label="FarHelm 显示名称" extra="留空恢复原名称。仅修改 FarHelm 显示，不会改动服务器目录。" rules={[{ max: 128 }]}><Input autoFocus /></Form.Item></Form>{model.save.error && <Alert type="error" title={errorText(model.save.error)} />}</Modal></>
}
