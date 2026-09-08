import { QueryClientProvider } from '@tanstack/react-query'
import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter } from 'react-router-dom'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { queryClient } from '../api/cache'
import type { AgentsState } from '../hooks/useAgents'
import { Overview } from './Overview'

const health = { state: 'online', data: { status: 'ok', service: 'farhelm-hub', version: '0.8.0', protocol: 'farhelm/1' } } as const
const counts = { active_experiments: 2, active_sessions: 3, unread_notifications: 7, failed_tasks: 4 }
const noAgents: AgentsState = { state: 'ready', data: { protocol: 'farhelm/1', agents: [] } }

function renderOverview(agents = noAgents, onRefresh = vi.fn()) {
  render(<MemoryRouter><QueryClientProvider client={queryClient}><Overview health={health} agents={agents} onRefresh={onRefresh} /></QueryClientProvider></MemoryRouter>)
  return onRefresh
}

afterEach(() => { vi.unstubAllGlobals() })

describe('Overview', () => {
  it('links authoritative counts to their source pages and separates Agent from Codex readiness', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: true, json: () => Promise.resolve(counts) }))
    renderOverview({ state: 'ready', data: { protocol: 'farhelm/1', agents: [
      { agent_id: 'titan', hostname: 'trainer-titan', online: true, agent_version: '0.8.0', last_seen_unix: 1_788_400_000, credential_state: 'paired', codex: { state: 'login_required' } },
      { agent_id: 'lab', hostname: 'offline-lab', online: false, agent_version: '0.8.0', last_seen_unix: 1_788_400_000, credential_state: 'paired' },
    ] } })
    const activity = screen.getByRole('navigation', { name: '工作动态' })
    expect(await within(activity).findByRole('link', { name: /活动实验.*2/ })).toHaveAttribute('href', '/experiments')
    expect(within(activity).getByRole('link', { name: /活动会话.*3/ })).toHaveAttribute('href', '/codex')
    expect(within(activity).getByRole('link', { name: /未读通知.*7/ })).toHaveAttribute('href', '/notifications')
    expect(within(activity).getByRole('link', { name: /失败.*4.*包含历史结果/ })).toHaveAttribute('href', '/notifications')
    expect(screen.getByText('trainer-titan')).toBeVisible()
    expect(screen.getByText('Codex 需要本机登录')).toBeVisible()
    expect(screen.getByText('1 台在线 · 1 台离线')).toBeVisible()
    expect(screen.queryByText('offline-lab')).not.toBeInTheDocument()
  })

  it('refreshes counts and parent status together, retaining labeled cached counts on failure', async () => {
    const fetcher = vi.fn().mockResolvedValueOnce({ ok: true, json: () => Promise.resolve(counts) }).mockRejectedValue(new Error('connection refused'))
    vi.stubGlobal('fetch', fetcher)
    const onRefresh = renderOverview()
    await screen.findByRole('link', { name: /活动实验.*2/ })
    await userEvent.click(screen.getByRole('button', { name: '刷新状态' }))
    expect(onRefresh).toHaveBeenCalledOnce()
    expect(await screen.findByText('统计刷新失败，当前显示上次结果')).toBeVisible()
    expect(screen.getByRole('link', { name: /活动实验.*2/ })).toBeVisible()
    expect(fetcher).toHaveBeenCalledTimes(2)
    expect(fetcher.mock.calls[0][1].signal).toBeInstanceOf(AbortSignal)
  })

  it('does not turn missing statistics or offline servers into a healthy empty state', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: true, json: () => Promise.resolve({ ...counts, active_experiments: -1 }) }))
    renderOverview({ state: 'ready', data: { protocol: 'farhelm/1', agents: [{ agent_id: 'lab', hostname: 'lab', online: false, agent_version: '0.8.0', last_seen_unix: 1_788_400_000, credential_state: 'paired' }] } })
    expect(await screen.findByText('业务统计暂时不可用')).toBeVisible()
    expect(screen.getAllByLabelText('数量不可用')).toHaveLength(4)
    expect(screen.getByText('当前没有在线服务器')).toBeVisible()
    expect(screen.queryByText('连接你的第一台服务器')).not.toBeInTheDocument()
  })
})
