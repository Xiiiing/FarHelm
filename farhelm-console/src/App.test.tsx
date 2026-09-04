import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter } from 'react-router-dom'
import { afterEach, describe, expect, it, vi } from 'vitest'

import App from './App'

const healthy = {
  status: 'ok',
  service: 'farhelm-hub',
  version: '0.1.0',
  protocol: 'farhelm/1',
}

const noAgents = { protocol: 'farhelm/1', agents: [] }
const session = { authenticated: true, user: 'admin', csrf_token: 'csrf-test', expires_at_unix: 2_000_000_000 }

function mockApi(agentResponse: unknown = noAgents) {
  return vi.fn().mockImplementation((input: RequestInfo | URL) => {
    const url = String(input)
    return Promise.resolve({
      ok: true,
      status: 200,
      json: () => Promise.resolve(url.endsWith('/api/v1/auth/session') ? session : url.endsWith('/api/v1/agents') ? agentResponse : url.endsWith('/api/v1/projects') ? { protocol: 'farhelm/1', projects: [] } : healthy),
    })
  })
}

afterEach(() => {
  vi.unstubAllGlobals()
  localStorage.clear()
})

describe('FarHelm Console', () => {
  it('uses password-only login without a TOTP field', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: false, status: 401 }))
    render(<MemoryRouter><App /></MemoryRouter>)
    expect(await screen.findByLabelText('用户名')).toBeInTheDocument()
    expect(screen.getByLabelText('密码')).toBeInTheDocument()
    expect(screen.queryByText(/TOTP|验证码|恢复码/)).not.toBeInTheDocument()
  })

  it('shows only validated Hub health data', async () => {
    vi.stubGlobal('fetch', mockApi())
    render(<MemoryRouter><App /></MemoryRouter>)

    expect(screen.getByText('正在恢复安全会话…')).toBeInTheDocument()
    expect(await screen.findByText('在线 · 已验证')).toBeInTheDocument()
    expect(screen.getByText('farhelm/1')).toBeInTheDocument()
  })

  it('shows an in-place error when Hub cannot be reached', async () => {
    vi.stubGlobal('fetch', vi.fn().mockImplementation((input: RequestInfo | URL) => String(input).endsWith('/api/v1/auth/session') ? Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(session) }) : Promise.reject(new Error('connection refused'))))
    render(<MemoryRouter><App /></MemoryRouter>)

    expect(await screen.findByText('Hub 当前不可用')).toBeInTheDocument()
    expect(screen.getByText('connection refused')).toBeInTheDocument()
  })

  it('persists an explicit theme choice', async () => {
    vi.stubGlobal('fetch', vi.fn().mockImplementation((input: RequestInfo | URL) => String(input).endsWith('/api/v1/auth/session') ? Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(session) }) : Promise.reject(new Error('offline'))))
    const user = userEvent.setup()
    render(<MemoryRouter><App /></MemoryRouter>)

    await user.click(await screen.findByRole('button', { name: '切换到深色主题' }))
    await waitFor(() => expect(localStorage.getItem('farhelm-color-mode')).toBe('dark'))
    expect(document.documentElement.dataset.theme).toBe('dark')
  })

  it('renders real Agent heartbeat data', async () => {
    vi.stubGlobal('fetch', mockApi({
      protocol: 'farhelm/1',
      agents: [{
        agent_id: 'gpu-a',
        hostname: 'trainer-a',
        agent_version: '0.1.0',
        last_seen_unix: 1_788_400_000,
        online: true,
        credential_state: 'paired',
      }],
    }))
    render(<MemoryRouter initialEntries={['/agents']}><App /></MemoryRouter>)

    expect(await screen.findByText('trainer-a')).toBeInTheDocument()
    expect(screen.getByText('gpu-a')).toBeInTheDocument()
    expect(screen.getByText('在线')).toBeInTheDocument()
  })

  it('rejects malformed Agent data without rendering it', async () => {
    vi.stubGlobal('fetch', mockApi({
      protocol: 'farhelm/1',
      agents: [{ agent_id: 'gpu-a', last_seen_unix: 'not-a-number' }],
    }))
    render(<MemoryRouter initialEntries={['/agents']}><App /></MemoryRouter>)

    expect(await screen.findByText('无法读取 Agent 列表')).toBeInTheDocument()
    expect(screen.queryByText('gpu-a')).not.toBeInTheDocument()
  })
})
