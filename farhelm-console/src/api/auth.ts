export type BrowserSession = {
  authenticated: true
  user: string
  csrf_token: string
  expires_at_unix: number
}

export async function readSession(): Promise<BrowserSession | null> {
  const response = await fetch('/api/v1/auth/session', { credentials: 'same-origin' })
  if (response.status === 401) return null
  if (!response.ok) throw new Error(`Hub returned HTTP ${response.status}`)
  const value = (await response.json()) as Partial<BrowserSession>
  if (value.authenticated !== true || typeof value.user !== 'string' || typeof value.csrf_token !== 'string' || typeof value.expires_at_unix !== 'number') {
    throw new Error('Hub returned an invalid session')
  }
  return value as BrowserSession
}

export async function login(username: string, password: string, totp: string): Promise<BrowserSession> {
  const response = await fetch('/api/v1/auth/login', {
    method: 'POST', credentials: 'same-origin', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username, password, totp }),
  })
  if (!response.ok) throw new Error(response.status === 429 ? '登录尝试过多，请稍后再试' : '用户名、密码或验证码不正确')
  const session = (await response.json()) as BrowserSession
  if (session.authenticated !== true || typeof session.csrf_token !== 'string') throw new Error('Hub returned an invalid session')
  return session
}

export async function logout(csrfToken: string): Promise<void> {
  const response = await fetch('/api/v1/auth/logout', { method: 'POST', credentials: 'same-origin', headers: { 'X-CSRF-Token': csrfToken } })
  if (!response.ok && response.status !== 401) throw new Error(`Hub returned HTTP ${response.status}`)
}
