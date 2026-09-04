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
          },
        ],
      }),
    }),
  )
  await page.route('**/api/v1/experiments', (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify({ protocol: 'farhelm/1', experiments: [{ watch_id: 'watch-a', agent_id: 'gpu-a', project_id: 'cc08', name: 'exp42', pid: 12345, state: 'succeeded', updated_at_unix: 2_000_000_000 }] }),
  }))
  await page.route('**/api/v1/codex/sessions', (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify({ protocol: 'farhelm/1', sessions: [{ session_id: 'ses-a', agent_id: 'gpu-a', project_id: 'cc08', mode: 'inspect', state: 'idle', updated_at_unix: 2_000_000_000 }] }),
  }))
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
  await page.getByLabel('下一条指令').fill('继续分析结果')
  await page.getByRole('button', { name: '发送' }).click()
  await expect.poll(() => sent).toEqual({ prompt: '继续分析结果', delivery: 'queue' })
})

test('notification page exposes explicit device opt-in', async ({ page }) => {
  await page.goto('/notifications')
  await expect(page.getByRole('heading', { name: '通知' })).toBeVisible()
  await expect(page.getByText(/不包含日志、源码或 prompt/)).toBeVisible()
  await expect(page.getByRole('button', { name: /启用此设备通知|关闭此设备通知/ })).toBeVisible()
})
