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

afterEach(() => {
  vi.unstubAllGlobals()
  localStorage.clear()
})

describe('FarHelm Console', () => {
  it('shows only validated Hub health data', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: true, json: () => Promise.resolve(healthy) }))
    render(<MemoryRouter><App /></MemoryRouter>)

    expect(screen.getByText('正在验证协议与服务状态…')).toBeInTheDocument()
    expect(await screen.findByText('在线 · 已验证')).toBeInTheDocument()
    expect(screen.getByText('farhelm/1')).toBeInTheDocument()
  })

  it('shows an in-place error when Hub cannot be reached', async () => {
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('connection refused')))
    render(<MemoryRouter><App /></MemoryRouter>)

    expect(await screen.findByText('Hub 当前不可用')).toBeInTheDocument()
    expect(screen.getByText('connection refused')).toBeInTheDocument()
  })

  it('persists an explicit theme choice', async () => {
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('offline')))
    const user = userEvent.setup()
    render(<MemoryRouter><App /></MemoryRouter>)

    await user.click(screen.getByRole('button', { name: '切换到深色主题' }))
    await waitFor(() => expect(localStorage.getItem('farhelm-color-mode')).toBe('dark'))
    expect(document.documentElement.dataset.theme).toBe('dark')
  })
})
