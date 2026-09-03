import AxeBuilder from '@axe-core/playwright'
import { expect, test } from '@playwright/test'

test.beforeEach(async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' })
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
