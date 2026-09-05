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
  await page.route('**/api/v1/codex/sessions/ses-a/transcript?*', (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ session_id: 'ses-a', turns: [{ turn_id: 'turn-a', status: 'completed', items: [{ item_id: 'user-a', kind: 'user_message', text: '检查训练结果' }, { item_id: 'agent-a', kind: 'assistant_message', text: '结果正常' }] }] }) }))
  await page.route('**/api/v1/codex/schedules?*', (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ protocol: 'farhelm/1', schedules: [] }) }))
  await page.route('**/api/v1/events/stream', (route) => route.fulfill({ status: 200, contentType: 'text/event-stream', body: '' }))
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
  await expect(page.getByText('ses-a').first()).toBeVisible()
  await expect(page.getByText('结果正常')).toBeVisible()
  await page.getByLabel('给 Codex 发送指令').fill('继续分析结果')
  await page.getByRole('button', { name: '发送指令' }).click()
  await expect.poll(() => sent).toEqual({ prompt: '继续分析结果', delivery: 'queue' })
  await expect(page.locator('.app-sider')).toHaveCount(0)

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

test('notification page exposes explicit device opt-in', async ({ page }) => {
  await page.goto('/notifications')
  await expect(page.getByRole('heading', { name: '通知' })).toBeVisible()
  await expect(page.getByText(/不包含日志、源码或 prompt/)).toBeVisible()
  await expect(page.getByRole('button', { name: /启用此设备通知|关闭此设备通知/ })).toBeVisible()
})
