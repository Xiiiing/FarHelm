import { useEffect, useRef, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Alert, Button, Form, Input, Modal, Skeleton } from 'antd'
import { CopyOutlined, ReloadOutlined } from '@ant-design/icons'
import { fetchNativeIdentity, handoffSession, renameSession, waitForCommand, type CodexSession } from '../../api/features'
import { keys, queryClient } from '../../api/cache'
import { errorText, formalTitle, sessionName } from './presentation'

export function RenameSession({ session, csrf, onClose }: { session: CodexSession; csrf: string; onClose: () => void }) {
  const [busy, setBusy] = useState(false), [error, setError] = useState<string>()
  const active = useRef(true)
  useEffect(() => { active.current = true; return () => { active.current = false } }, [])
  return <Modal title="重命名会话" open onCancel={onClose} footer={null} destroyOnHidden>
    <Form layout="vertical" initialValues={{ name: formalTitle(session.title) ? session.title : '' }} onFinish={async ({ name }: { name: string }) => {
      setBusy(true); setError(undefined)
      try {
        await waitForCommand(await renameSession(csrf, session.session_id, name.trim()))
        await queryClient.invalidateQueries({ queryKey: keys.session(session.session_id), exact: true })
        void queryClient.invalidateQueries({ queryKey: ['codex', 'sessions'] })
        void queryClient.invalidateQueries({ queryKey: ['codex', 'native', session.session_id] })
        if (active.current) onClose()
      } catch (reason) { if (active.current) setError(errorText(reason)) } finally { if (active.current) setBusy(false) }
    }}>
      <p className="settings-detail-note">修改服务器上的原生 Codex 名称，方便在其他客户端找到同一条会话。</p>
      {error && <Alert type="warning" title={error} showIcon />}
      <Form.Item name="name" label="会话名称" rules={[{ required: true, whitespace: true, message: '请输入会话名称' }, { max: 128 }, { pattern: /^[^/\\]+$/, message: '名称不能包含路径分隔符' }]}><Input autoFocus maxLength={128} placeholder={sessionName(session)} disabled={busy} /></Form.Item>
      <Button type="primary" htmlType="submit" block loading={busy}>保存到 Codex</Button>
    </Form>
  </Modal>
}

export function NativeSession({ session, csrf, onClose, onRename }: { session: CodexSession; csrf: string; onClose: () => void; onRename: () => void }) {
  const info = useQuery({ queryKey: ['codex', 'native', session.session_id], queryFn: ({ signal }) => fetchNativeIdentity(session.session_id, signal), refetchOnMount: 'always' })
  const [copied, setCopied] = useState(false), [error, setError] = useState<string>()
  const [busy, setBusy] = useState(false), [released, setReleased] = useState(false)
  const active = useRef(true)
  useEffect(() => { active.current = true; return () => { active.current = false } }, [])
  const command = /^[A-Za-z0-9_-]+$/.test(session.session_id) ? `codex resume ${session.session_id}` : undefined
  return <Modal title="在原生 Codex 中继续" open onCancel={onClose} footer={<Button onClick={onClose}>完成</Button>}>
    {info.isPending ? <Skeleton active paragraph={{ rows: 3 }} /> : info.error ? <Alert type="warning" showIcon title="暂时无法核对原生会话" description={errorText(info.error)} action={<Button icon={<ReloadOutlined />} onClick={() => void info.refetch()}>重试</Button>} /> : <div className="native-session-details">
      <Alert type={info.data?.persisted ? 'success' : 'info'} showIcon title={info.data?.persisted ? '会话已保存在服务器 Codex 中' : '原生会话尚未落盘'} description={info.data?.persisted ? '请在连接同一服务器、同一用户的 Codex 客户端中打开。' : '先发送一条消息并等待处理完成，再核对保存状态。'} />
      <dl><dt>服务器 · 项目</dt><dd>{session.agent_id} · {session.project_id}</dd><dt>原生名称</dt><dd>{info.data?.native_name || '未设置名称，Codex 使用消息摘要'}</dd><dt>会话 ID</dt><dd>{session.session_id}</dd></dl>
      {info.data?.native_name && !formalTitle(info.data.native_name) && <p>旧版本使用了通用名称，可能与网页显示的摘要不同。<Button type="link" onClick={onRename}>重命名会话</Button></p>}
      {info.data?.held_by_agent && <div><p>FarHelm 仍持有原生会话连接。切换客户端前可释放此 Agent 的空闲 Codex 连接；有活动任务、后台终端或未保存会话时不会交接。</p><Button loading={busy} disabled={!info.data.persisted || !!session.active_turn_id || info.isFetching} onClick={async () => {
        setBusy(true); setError(undefined)
        try {
          await waitForCommand(await handoffSession(csrf, session.session_id))
          if (active.current) { setReleased(true); await info.refetch() }
        } catch (reason) { if (active.current) setError(errorText(reason)) }
        finally { if (active.current) setBusy(false) }
      }}>释放连接后继续</Button></div>}
      {released && <p role="status">已释放 FarHelm 的空闲连接，可以在原生客户端继续。</p>}
      {command && <><p>在该服务器的终端中继续：</p><div className="native-resume-command"><code>{command}</code><Button icon={<CopyOutlined />} aria-label="复制 Codex 恢复命令" onClick={() => { void navigator.clipboard.writeText(command).then(() => setCopied(true)).catch(() => setError('复制失败，请手动复制上面的命令')) }}>{copied ? '已复制' : '复制'}</Button></div></>}
      <p className="settings-detail-note">桌面端请重新打开对应项目或刷新会话列表。列表有筛选时，可用同一会话 ID 定位；FarHelm 不会复制会话到其他服务器或账户。</p>
      <p className="settings-detail-note">回到网页续写前，请关闭原生客户端的会话连接。网页再次发送或定时任务执行时会尝试恢复会话。</p>
      {error && <Alert type="warning" title={error} />}
    </div>}
  </Modal>
}
