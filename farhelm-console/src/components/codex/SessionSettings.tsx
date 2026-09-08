import { DownOutlined, EyeOutlined, SafetyOutlined } from '@ant-design/icons'
import { Button, Popover } from 'antd'
import { cloneElement, useEffect, useState, type ReactElement, type ReactNode } from 'react'
import type { CodexSession, SessionContext } from '../../api/features'

function SettingsPopover({ title, placement, content, children }: { title: string; placement: 'topLeft' | 'topRight'; content: ReactNode; children: ReactElement<{ 'aria-expanded'?: boolean }> }) {
  const [open, setOpen] = useState(false)
  useEffect(() => {
    if (!open) return
    const dismiss = (event: KeyboardEvent) => { if (event.key === 'Escape') setOpen(false) }
    document.addEventListener('keydown', dismiss)
    return () => document.removeEventListener('keydown', dismiss)
  }, [open])
  return <Popover trigger="click" open={open} onOpenChange={setOpen} placement={placement} title={title} content={content} destroyOnHidden fresh>{cloneElement(children, { 'aria-expanded': open })}</Popover>
}

const efforts: Record<string, string> = { none: '无', minimal: '最低', low: '低', medium: '中', high: '高', xhigh: '极高', max: '最高', ultra: '超高' }
const permissions: Record<string, { name: string; description: string }> = {
  'read-only': { name: '仅分析', description: 'Codex 当前使用只读沙盒，可以读取和分析文件。修改文件需要更改服务器上的会话权限。' },
  'workspace-write': { name: '可编辑', description: 'Codex 可以在当前工作区修改文件并运行命令；工作区外访问受服务器上的权限规则限制。' },
  'danger-full-access': { name: '完全访问', description: 'Codex 沿用服务器确认的完全访问设置，可以访问当前系统账户可访问的文件与网络。' },
  'external-sandbox': { name: '外部沙盒', description: '文件与网络访问由服务器的外部沙盒控制。' },
  custom: { name: '自定义权限', description: '此会话使用服务器配置的自定义权限。' },
}
const approvals: Record<string, string> = { never: '无需人工批准', 'on-request': '按需申请批准', untrusted: '非信任操作需批准', 'on-failure': '失败后申请批准', custom: '自定义审批规则' }

export function PermissionDetails({ context, session, supported }: { context?: SessionContext; session?: CodexSession; supported?: boolean }) {
  const legacy = !!session && supported === false && !context
  const sandbox = legacy ? session.mode === 'edit' ? 'workspace-write' : 'read-only' : context?.sandbox
  const permission = sandbox ? permissions[sandbox] : undefined
  const name = permission?.name ?? '跟随 Codex'
  const approval = legacy ? 'on-request' : context?.approval_policy
  return <SettingsPopover placement="topLeft" title="会话权限" content={<div className="session-settings-details">
    <p>{permission?.description ?? '沿用这条会话在服务器上的 Codex 权限。发送时由 Codex 恢复设置，确认后在这里显示具体权限。'}</p>
    {approval && <dl><dt>审批方式</dt><dd>{approvals[approval] ?? '由 Codex 管理'}</dd>{context?.approvals_reviewer && <><dt>审批处理</dt><dd>{context.approvals_reviewer === 'user' ? '由用户确认' : 'Codex 自动审查'}</dd></>}</dl>}
    <p className="settings-detail-note">{legacy ? '当前 Agent 使用旧的权限逻辑，请升级 Agent 后使用会话权限继承。' : '权限由服务器上的 Codex 管理，此处展示实际设置。'}</p>
    {approval !== 'never' && context?.approvals_reviewer !== 'auto_review' && context?.approvals_reviewer !== 'guardian_subagent' && <p className="settings-detail-note">网页暂不支持人工审批；需要你批准的操作会被拒绝，不会自动放行。</p>}
  </div>}>
    <Button type="text" className="composer-control permission-action" disabled={!session} aria-label={`会话权限：${name}`} icon={sandbox === 'read-only' ? <EyeOutlined /> : <SafetyOutlined />}><span className="permission-label">{name}</span><span className="permission-short" aria-hidden>权限</span><DownOutlined className="control-chevron" /></Button>
  </SettingsPopover>
}

export function ModelDetails({ context, loading, disabled }: { context?: SessionContext; loading: boolean; disabled: boolean }) {
  const model = context?.model
  const effort = context?.reasoning_effort ? efforts[context.reasoning_effort] ?? context.reasoning_effort : undefined
  const label = model ?? (loading ? '读取模型…' : '模型未提供')
  return <SettingsPopover placement="topRight" title="会话模型" content={<div className="session-settings-details">
    <dl><dt>模型</dt><dd>{label}</dd><dt>推理强度</dt><dd>{effort ?? 'Codex 未提供'}</dd></dl>
    <p className="settings-detail-note">{model ? '来自 Codex 当前会话或最近保存的配置。模型与推理强度在服务器上的 Codex 中调整。' : '当前 Codex 或 Agent 尚未返回模型信息。不会根据版本号猜测模型；刷新对话可重新读取。'}</p>
  </div>}>
    <Button type="text" className="composer-control model-action" disabled={disabled} aria-label={`会话模型：${label}${effort ? `，推理强度${effort}` : ''}`}><span className="model-label">{label}</span>{effort && <span className="model-effort">{effort}</span>}<DownOutlined className="control-chevron" /></Button>
  </SettingsPopover>
}
