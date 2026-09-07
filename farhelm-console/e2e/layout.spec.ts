import AxeBuilder from '@axe-core/playwright'
import { expect, test } from '@playwright/test'

test.beforeEach(async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' })
  await page.route('**/api/v1/auth/session', (route) =>
    route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify({ authenticated: true, user: 'admin', csrf_token: 'csrf-test', expires_at_unix: 2_000_000_000 }),
    }),
  )
  await page.route('**/api/v1/health', (route) =>
    route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify({ status: 'ok', service: 'farhelm-hub', version: '0.1.0', protocol: 'farhelm/1' }),
    }),
  )
  await page.route('**/api/v1/agents', (route) =>
    route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify({
        protocol: 'farhelm/1',
        agents: [
          {
            agent_id: 'gpu-a',
            hostname: 'trainer-a',
            agent_version: '0.1.0',
            last_seen_unix: Math.floor(Date.now() / 1000),
            online: true,
            credential_state: 'paired',
          },
        ],
      }),
    }),
  )
  await page.route('**/api/v1/experiments', (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify({ protocol: 'farhelm/1', experiments: [{ watch_id: 'watch-a', agent_id: 'gpu-a', project_id: 'cc08', name: 'exp42', pid: 12345, state: 'watching', updated_at_unix: 2_000_000_000 }] }),
  }))
  await page.route('**/api/v1/projects', (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ protocol: 'farhelm/1', projects: [] }) }))
  await page.route('**/api/v1/codex/sessions?*', (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify({ protocol: 'farhelm/1', sessions: [{ session_id: 'ses-a', agent_id: 'gpu-a', project_id: 'cc08', mode: 'inspect', state: 'idle', updated_at_unix: 2_000_000_000 }] }),
  }))
  await page.route('**/api/v1/codex/session-display', (route) => route.fulfill({ json: { protocol: 'farhelm/1', sessions: [], incomplete_agents: [] } }))
  await page.route(/\/api\/v1\/codex\/sessions\/ses-[ab]$/, (route) => route.fulfill({ json: { session_id: route.request().url().split('/').at(-1), agent_id: 'gpu-a', project_id: 'cc08', mode: 'inspect', state: 'idle', updated_at_unix: 2_000_000_000 } }))
  await page.route('**/api/v1/codex/sessions/ses-a/transcript?*', (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ session_id: 'ses-a', turns: [{ turn_id: 'turn-a', status: 'completed', items: [{ item_id: 'user-a', kind: 'user_message', text: '检查训练结果' }, { item_id: 'agent-a', kind: 'assistant_message', text: '结果正常' }] }] }) }))
  await page.route('**/api/v1/codex/schedules?*', (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ protocol: 'farhelm/1', schedules: [] }) }))
  await page.route('**/api/v1/events/stream', (route) => route.fulfill({ status: 200, contentType: 'text/event-stream', body: '' }))
  await page.route('**/api/v1/experiment-runs?*', (route) => route.fulfill({ json: { protocol: 'farhelm/1', experiments: [{ id: 'watch-a', watch_id: 'watch-a', agent_id: 'gpu-a', project_id: 'cc08', name: 'exp42', pid: 12345, source: 'pid_watch', state: 'watching', updated_at_unix: 2_000_000_000 }] } }))
  await page.route('**/api/v1/notifications?*', (route) => route.fulfill({ json: { notifications: [], latest_id: 0, unread_count: 0 } }))
  await page.route('**/api/v1/notifications/preferences', (route) => route.fulfill({ json: { experiments: true, codex: true } }))
  await page.route('**/api/v1/overview', (route) => route.fulfill({ json: { active_experiments: 1, active_sessions: 0, unread_notifications: 0, failed_tasks: 0 } }))
  await page.route('**/api/v1/audit*', (route) => route.fulfill({ json: { entries: [], next_cursor: null } }))
  await page.goto('/')
})

test('responsive navigation and validated status are visible', async ({ page }, testInfo) => {
  await expect(page.getByRole('heading', { name: '运行总览' })).toBeVisible()
  await expect(page.getByText('在线 · 已验证')).toBeVisible()

  if (testInfo.project.name === 'mobile') {
    await expect(page.getByRole('navigation', { name: '主要导航' })).toBeVisible()
    await expect(page.locator('.app-sider')).toHaveCount(0)
  } else {
    await expect(page.locator('.app-sider')).toBeVisible()
    await expect(page.getByRole('navigation', { name: '主要导航' })).toHaveCount(0)
  }

  const results = await new AxeBuilder({ page }).analyze()
  expect(results.violations.filter((violation) => violation.impact === 'critical' || violation.impact === 'serious')).toEqual([])

  await page.screenshot({ path: testInfo.outputPath('overview.png'), fullPage: true })
})

test('keyboard navigation reaches visible controls', async ({ page }) => {
  await page.keyboard.press('Tab')
  const focused = page.locator(':focus-visible')
  await expect(focused).toHaveCount(1)
})

test('agent page renders validated Hub data', async ({ page }) => {
  await page.goto('/agents')
  await expect(page.getByRole('heading', { name: '服务器' })).toBeVisible()
  await expect(page.getByText('trainer-a')).toBeVisible()
  await expect(page.getByText('gpu-a')).toBeVisible()
  await expect(page.getByText('在线', { exact: true })).toBeVisible()
})

test('server pairing and project import need no long token or path', async ({ page }) => {
  await page.route('**/api/v1/agents/pairing-codes', async (route) => {
    expect(route.request().postDataJSON()).toEqual({ agent_id: 'titan' })
    await route.fulfill({ contentType: 'application/json', body: JSON.stringify({ protocol: 'farhelm/1', pairing_id: 'pair-a', agent_id: 'titan', code: '12345678', expires_at_unix: 2_000_000_000 }) })
  })
  await page.goto('/agents')
  await page.getByRole('button', { name: '添加服务器' }).click()
  await page.getByLabel('Agent 名称').fill('titan')
  await page.getByRole('button', { name: '生成配对码' }).click()
  await expect(page.getByLabel('配对码 12345678')).toContainText('1234 5678')
  await expect(page.getByText(/长 Token 不会显示/)).toBeVisible()
})

test('experiment deep link and Codex manual queue use the mobile-safe workflow', async ({ page }) => {
  await page.goto('/experiments?watch=watch-a')
  await expect(page.getByRole('heading', { name: '实验' })).toBeVisible()
  await expect(page.getByText('exp42')).toBeVisible()
  await expect(page.locator('.agent-row.highlighted')).toHaveCount(1)
  await expect(page.getByRole('button', { name: /启动|停止|重启/ })).toHaveCount(0)

  let sent: unknown
  await page.route('**/api/v1/codex/sessions/ses-a/messages', async (route) => {
    sent = route.request().postDataJSON()
    await route.fulfill({ status: 200, contentType: 'application/json', body: '{}' })
  })
  await page.goto('/codex?session=ses-a')
  await page.getByRole('button', { name: '会话操作' }).click()
  await expect(page.getByRole('menuitem', { name: '复制会话 ID' })).toBeVisible()
  await page.keyboard.press('Escape')
  await expect(page.getByText('结果正常')).toBeVisible()
  await page.getByLabel('给 Codex 发送指令').fill('继续分析结果')
  await page.getByRole('button', { name: '发送指令' }).click()
  await expect.poll(() => sent).toEqual({ prompt: '继续分析结果', delivery: 'queue' })
  if (page.viewportSize()!.width >= 768) await expect(page.locator('.app-sider')).toBeVisible()
  else await expect(page.getByRole('navigation', { name: '主要导航' })).toBeVisible()

  let scheduled: unknown
  await page.route('**/api/v1/codex/sessions/ses-a/schedules', async (route) => {
    scheduled = route.request().postDataJSON()
    await route.fulfill({ status: 202, contentType: 'application/json', body: '{}' })
  })
  await page.getByRole('button', { name: /定时发送/ }).click()
  await page.getByLabel('发送时间（本地时区）').fill('2030-01-01T12:00')
  await page.getByLabel('指令', { exact: true }).fill('定时检查结果')
  await page.getByRole('button', { name: '创建定时任务' }).click()
  await expect.poll(() => scheduled).toMatchObject({ prompt: '定时检查结果', trigger: { type: 'at_time' } })

  const results = await new AxeBuilder({ page }).analyze()
  expect(results.violations.filter((violation) => violation.impact === 'critical' || violation.impact === 'serious')).toEqual([])
})

test('notification page exposes browser notification preferences', async ({ page }) => {
  await page.goto('/notifications')
  await expect(page.getByRole('heading', { name: '通知' })).toBeVisible()
  await expect(page.getByText(/保持 FarHelm 页面打开/)).toBeVisible()
  await expect(page.getByRole('button', { name: '发送页面测试通知' })).toBeVisible()
})

test('browser reminders deduplicate IDs and do not toast restored history', async ({ page }) => {
  await page.addInitScript(() => {
    const sources: EventTarget[] = []
    class FakeEventSource extends EventTarget {
      constructor() { super(); sources.push(this); setTimeout(() => this.dispatchEvent(new Event('open')), 0) }
      close() { sources.splice(sources.indexOf(this), 1) }
    }
    Object.assign(window, { EventSource: FakeEventSource, emitNotice: (kind: string) => { for (const source of sources) source.dispatchEvent(new MessageEvent(kind, { data: JSON.stringify({ payload: { id: 2, agent_id: 'gpu-a', category: 'test', state: 'succeeded', title: '新的页面测试', created_at_unix: Math.floor(Date.now() / 1000) } }) })) } })
  })
  let latest = 1; let lists = 0
  await page.route('**/api/v1/notifications?*', (route) => {
    lists++
    return route.fulfill({ json: { latest_id: latest, unread_count: latest, preferences: { experiments: true, codex: true }, notifications: [{ id: latest, agent_id: 'gpu-a', category: 'test', state: 'succeeded', title: latest === 1 ? '历史记录' : '新的页面测试', created_at_unix: Math.floor(Date.now() / 1000) }] } })
  })
  await page.route('**/api/v1/notifications/test', (route) => { latest = 2; return route.fulfill({ json: { ok: true } }) })
  await page.goto('/settings')
  await expect.poll(() => lists).toBeGreaterThan(0)
  await expect(page.getByRole('switch', { name: '实验页面提醒' })).toBeEnabled()
  await expect(page.locator('.ant-notification-notice')).toHaveCount(0)
  await page.getByRole('button', { name: '发送页面测试通知' }).click()
  const emit = (kind: string) => page.evaluate((event) => { (window as unknown as { emitNotice: (kind: string) => void }).emitNotice(event) }, kind)
  await emit('notification.created')
  await expect(page.locator('.ant-notification-notice')).toHaveCount(1)
  await expect(page.locator('.ant-notification-notice')).toContainText('新的页面测试')
  await emit('notification.created'); await emit('open'); await emit('notification.created')
  await expect(page.locator('.ant-notification-notice')).toHaveCount(1)
})

test('switching sessions ignores late history responses', async ({ page }, testInfo) => {
  await page.route('**/api/v1/codex/sessions?*', (route) => route.fulfill({ json: { protocol: 'farhelm/1', sessions: ['ses-a', 'ses-b'].map((session_id) => ({ session_id, agent_id: 'gpu-a', project_id: 'cc08', mode: 'inspect', state: 'idle', title: session_id, updated_at_unix: 2000000000 })) } }))
  let release: () => void = () => {}; let returned = false
  const gate = new Promise<void>((resolve) => { release = resolve })
  await page.route('**/api/v1/codex/sessions/ses-a/transcript?*', async (route) => { await gate; await route.fulfill({ json: { session_id: 'ses-a', turns: [{ turn_id: 'old', status: 'completed', items: [{ item_id: 'old', kind: 'assistant_message', text: '迟到的旧会话正文' }] }] } }); returned = true })
  await page.route('**/api/v1/codex/sessions/ses-b/transcript?*', (route) => route.fulfill({ json: { session_id: 'ses-b', turns: [{ turn_id: 'new', status: 'completed', items: [{ item_id: 'new', kind: 'assistant_message', text: '当前会话正文' }] }] } }))
  await page.goto('/codex?session=ses-a')
  if (testInfo.project.name === 'mobile') await page.getByRole('button', { name: '打开会话列表' }).click()
  await page.getByRole('button', { name: /ses-b/ }).filter({ visible: true }).click()
  await expect(page.getByText('当前会话正文')).toBeVisible()
  release()
  await expect.poll(() => returned).toBe(true)
  await expect(page.getByText('迟到的旧会话正文')).toHaveCount(0)
  await expect(page.getByText('当前会话正文')).toBeVisible()
})

test('same project names on different Agents retain the selected target', async ({ page }, testInfo) => {
  await page.route('**/api/v1/projects', (route) => route.fulfill({ json: { protocol: 'farhelm/1', projects: ['gpu-a', 'gpu-b'].map((agent_id) => ({ candidate_id: `candidate-${agent_id}`, agent_id, display_name: 'shared', suggested_project_id: 'shared', session_count: 1, state: 'approved', updated_at_unix: 2000000000 })) } }))
  let target: unknown
  await page.route('**/api/v1/codex/sessions', async (route) => { target = route.request().postDataJSON(); await route.fulfill({ status: 202, json: {} }) })
  await page.goto('/codex')
  if (testInfo.project.name === 'mobile') await page.getByRole('button', { name: '打开会话列表' }).click()
  await page.getByRole('button', { name: '新建会话' }).filter({ visible: true }).click()
  await page.getByLabel('项目', { exact: true }).click()
  await page.getByText('shared · gpu-b', { exact: true }).click()
  await page.getByRole('dialog').getByRole('button', { name: /创\s*建/ }).click()
  await expect.poll(() => target).toEqual({ agent_id: 'gpu-b', project_id: 'shared', mode: 'inspect' })
})


test('failed sends preserve the draft and operation ID; IME Enter does not submit', async ({ page }) => {
  await page.goto('/codex?session=ses-a')
  await expect(page.getByText('结果正常')).toBeVisible()
  const keys: string[] = []
  await page.route('**/api/v1/codex/sessions/ses-a/messages', async (route) => {
    keys.push(route.request().headers()['idempotency-key'])
    await route.fulfill(keys.length === 1 ? { status: 504, json: { error: 'agent_save_unconfirmed' } } : { status: 202, json: { state: 'accepted' } })
  })
  const composer = page.getByLabel('给 Codex 发送指令')
  await composer.fill('检查🙂训练')
  await composer.dispatchEvent('keydown', { key: 'Enter', code: 'Enter', isComposing: true, keyCode: 229 })
  expect(keys).toHaveLength(0)
  await page.getByRole('button', { name: '发送指令' }).click()
  await expect(page.getByText(/尚未确认 Agent 保存/)).toBeVisible()
  await expect(composer).toHaveValue('检查🙂训练')
  await page.getByRole('button', { name: '发送指令' }).click()
  await expect(composer).toHaveValue('')
  expect(keys).toHaveLength(2); expect(keys[0]).toBe(keys[1])
})

for (const path of ['/notifications', '/settings', '/audit', '/experiments']) {
  test(`existing page ${path} remains accessible`, async ({ page }, testInfo) => {
    await page.goto(path)
    await expect(page.locator('h1').first()).toBeVisible()
    const results = await new AxeBuilder({ page }).analyze()
    expect(results.violations.filter((v) => v.impact === 'critical' || v.impact === 'serious')).toEqual([])
    await page.screenshot({ path: testInfo.outputPath(`${path.slice(1)}.png`), fullPage: true })
  })
}
