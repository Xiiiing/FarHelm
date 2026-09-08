import { expect, test, type Page } from '@playwright/test'

const notice = (id: number, title: string) => ({ id, event_id: `event-${id}`, agent_id: 'gpu-a', category: 'experiment', state: 'succeeded', target_id: `run-${id}`, title, message: '合成实验结果', created_at_unix: 2_000_000_000, read_at_unix: null as number | null })
const run = (id: number, name: string) => ({ id: `run-${id}`, agent_id: 'gpu-a', project_id: 'project', source: 'script_report', name, state: 'succeeded', updated_at_unix: 2_000_000_000 })
const session = { session_id: 'ses-a', agent_id: 'gpu-a', project_id: 'project', mode: 'inspect', state: 'idle', title: '测试会话', updated_at_unix: 2_000_000_000 }
const emit = (page: Page, type: string, payload = {}) => page.evaluate(({ type, payload }) => {
  (window as unknown as { metadataEvent: (type: string, payload: object) => void }).metadataEvent(type, payload)
}, { type, payload })

test.beforeEach(async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' })
  await page.addInitScript(() => {
    const sources = new Set<EventTarget>()
    class Source extends EventTarget { constructor() { super(); sources.add(this); setTimeout(() => this.dispatchEvent(new Event('open')), 0) } close() { sources.delete(this) } }
    Object.assign(window, { EventSource: Source, metadataEvent: (type: string, payload: object) => sources.forEach(source => source.dispatchEvent(new MessageEvent(type, { data: JSON.stringify({ payload }) }))) })
  })
  await page.route('**/api/v1/**', route => {
    const url = new URL(route.request().url()), path = url.pathname, more = url.searchParams.has('cursor')
    let value: unknown = {}
    if (path.endsWith('/project-preferences')) return route.fulfill({ json: { revision: 0, projects: [] } })
    if (path.endsWith('/auth/session')) value = { authenticated: true, user: 'admin', csrf_token: 'test-csrf', expires_at_unix: 2_000_000_000 }
    else if (path.endsWith('/agents')) value = { protocol: 'farhelm/1', agents: ['a', 'b'].map(id => ({ agent_id: `gpu-${id}`, hostname: `服务器 ${id}`, agent_version: '0.9.0', online: true, capabilities: ['codex.session_context'], last_seen_unix: 2_000_000_000, codex: { state: 'ready', version: '0.153.4' } })) }
    else if (path.endsWith('/health')) value = { protocol: 'farhelm/1', service: 'farhelm-hub', status: 'ok', version: '0.9.0' }
    else if (path.endsWith('/notifications/preferences')) value = { experiments: true, codex: true }
    else if (path.endsWith('/notifications')) value = { notifications: [notice(more ? 1 : 2, more ? '第二页记录' : '第一页记录')], latest_id: 2, unread_count: 2, next_cursor: more ? null : 2 }
    else if (path.endsWith('/experiment-runs')) value = { experiments: [run(more ? 1 : 2, more ? '第二页记录' : '第一页记录')], next_cursor: more ? null : 2 }
    else if (path.endsWith('/audit')) value = { entries: [{ id: more ? 1 : 2, action: more ? '第二页记录' : '第一页记录', target: 'synthetic', outcome: 'succeeded', created_at_unix: 2_000_000_000 }], next_cursor: more ? null : 2 }
    else if (path.endsWith('/codex/sessions')) value = { protocol: 'farhelm/1', sessions: [session] }
    else if (path.endsWith('/ses-a')) value = session
    else if (path.endsWith('/transcript')) value = { session_id: 'ses-a', turns: [] }
    else if (path.endsWith('/session-display')) value = { sessions: [], incomplete_agents: [] }
    else if (path.endsWith('/project-preferences')) value = { revision: 0, projects: [] }
    else if (path.endsWith('/projects')) value = { protocol: 'farhelm/1', projects: [] }
    else if (path.endsWith('/experiments')) value = { protocol: 'farhelm/1', experiments: [] }
    else if (path.endsWith('/schedules')) value = { protocol: 'farhelm/1', schedules: [] }
    return route.fulfill({ json: value })
  })
})

for (const [path, more] of [['notifications', '加载更多通知'], ['experiments', '加载更多实验'], ['audit', '加载更多记录']]) {
  test(`${path} refresh retains every loaded page and clears recovered errors`, async ({ page }) => {
    await page.goto(`/${path}`)
    await expect(page.getByText('第一页记录', { exact: true })).toBeVisible()
    await page.getByRole('button', { name: more }).click()
    await expect(page.getByText('第二页记录', { exact: true })).toBeVisible()
    const endpoint = path === 'experiments' ? 'experiment-runs' : path
    await page.route(`**/api/v1/${endpoint}?*`, route => route.fulfill({ status: 503, json: { error: 'synthetic_failure' } }))
    if (path === 'audit') await page.route('**/api/v1/audit', route => route.fulfill({ status: 503, json: { error: 'synthetic_failure' } }))
    await page.getByRole('button', { name: /^刷\s*新$/ }).click()
    await expect(page.getByRole('alert').first()).toBeVisible()
    await expect(page.getByText('第二页记录', { exact: true })).toBeVisible()
    await page.unroute(`**/api/v1/${endpoint}?*`)
    if (path === 'audit') await page.unroute('**/api/v1/audit')
    await page.getByRole('button', { name: /^刷\s*新$/ }).click()
    await expect(page.getByRole('alert')).toHaveCount(0)
    await expect(page.getByText('第二页记录', { exact: true })).toBeVisible()
  })
}

test('all registered Agents remain available outside the current page; obsolete errors cannot replace the new filter', async ({ page }) => {
  let release: () => void = () => {}
  const pending = new Promise<void>(resolve => { release = resolve })
  await page.route('**/api/v1/experiment-runs?*', async route => {
    const agent = new URL(route.request().url()).searchParams.get('agent')
    if (agent === 'gpu-a') { await pending; return route.fulfill({ status: 503, json: { error: 'old_filter' } }).catch(() => {}) }
    return route.fulfill({ json: { experiments: [run(1, agent === 'gpu-b' ? '服务器 b 的结果' : '当前结果')], next_cursor: null } })
  })
  await page.goto('/experiments')
  await page.getByRole('combobox', { name: '实验服务器' }).click()
  await expect(page.getByText('服务器 b', { exact: true }).last()).toBeVisible()
  await page.getByText('服务器 a', { exact: true }).last().click()
  await page.getByRole('combobox', { name: '实验服务器' }).click()
  await page.getByText('服务器 b', { exact: true }).last().click()
  await expect(page.getByText('服务器 b 的结果', { exact: true })).toBeVisible()
  release(); await page.waitForTimeout(150)
  await expect(page.getByRole('alert')).toHaveCount(0)
})

test('notification detail exposes read errors and retries without repeating read writes on filtering', async ({ page }) => {
  const item = notice(2, '通知详情内容'); let fail = true, writes = 0
  await page.route('**/api/v1/notifications/2', route => fail ? route.fulfill({ status: 503, json: {} }) : route.fulfill({ json: { notification: item, deliveries: [] } }))
  await page.route('**/api/v1/notifications/2/read', route => { writes++; item.read_at_unix = 2_000_000_001; return route.fulfill({ json: {} }) })
  await page.goto('/notifications?id=2')
  const drawer = page.getByRole('dialog', { name: '通知详情', exact: true })
  await expect(drawer).toBeVisible(); await expect(drawer.getByText('无法读取通知详情')).toBeVisible()
  fail = false; await drawer.getByRole('button', { name: '重试读取' }).click()
  await expect(drawer.getByRole('heading', { name: '通知详情内容' })).toBeVisible()
  await expect.poll(() => writes).toBe(1)
  await drawer.getByRole('button', { name: 'Close' }).click()
  await page.getByRole('button', { name: '仅未读' }).click()
  await page.getByRole('button', { name: '查看详情' }).first().click()
  await expect(drawer.getByRole('heading', { name: '通知详情内容' })).toBeVisible()
  expect(writes).toBe(1)
})

test('notification projection events refresh loaded pages without relying on a turn event', async ({ page }) => {
  await page.goto('/notifications'); await page.getByRole('button', { name: '加载更多通知' }).click()
  await expect(page.getByText('第二页记录', { exact: true })).toBeVisible()
  await page.route('**/api/v1/notifications?*', route => route.fulfill({ json: { notifications: new URL(route.request().url()).searchParams.has('cursor') ? [notice(1, '第二页记录')] : [notice(3, '刚完成的实验'), notice(2, '第一页记录')], latest_id: 3, unread_count: 3, next_cursor: new URL(route.request().url()).searchParams.has('cursor') ? null : 2 } }))
  await emit(page, 'notification.created', notice(3, '刚完成的实验'))
  await expect(page.getByText('刚完成的实验', { exact: true })).toBeVisible()
  await expect(page.getByText('第二页记录', { exact: true })).toBeVisible()
})

test('created schedules appear immediately and cancellation requires the task confirmation', async ({ page }) => {
  let created = false, cancelled = false
  await page.route('**/api/v1/codex/schedules?*', route => route.fulfill({ json: { protocol: 'farhelm/1', schedules: created ? [{ schedule_id: 'schedule-a', session_id: 'ses-a', agent_id: 'gpu-a', project_id: 'project', trigger: { type: 'at_time', run_at_unix: 2_000_000_000 }, state: cancelled ? 'cancelled' : 'pending' }] : [] } }))
  await page.route('**/api/v1/codex/sessions/ses-a/schedules', route => { created = true; return route.fulfill({ json: { state: 'completed' } }) })
  await page.route('**/api/v1/codex/schedules/schedule-a/cancel', route => { cancelled = true; return route.fulfill({ json: { state: 'completed' } }) })
  await page.goto('/codex?session=ses-a')
  const openList = async () => { await page.getByRole('button', { name: '会话操作' }).click(); await page.getByRole('menuitem', { name: '定时任务' }).click() }
  await openList(); await expect(page.getByText('当前会话没有定时任务')).toBeVisible()
  await page.getByRole('dialog').getByRole('button', { name: 'Close' }).click()
  await page.getByRole('button', { name: '定时发送' }).click()
  await page.getByLabel('发送时间（本地时区）').fill('2020-01-01T12:00')
  await page.getByLabel('指令', { exact: true }).fill('合成调度指令')
  await page.getByRole('button', { name: '创建定时任务', exact: true }).click()
  await expect(page.getByText('发送时间必须在 60 秒至 365 天之后')).toBeVisible()
  expect(created).toBe(false)
  const localTime = await page.evaluate(() => { const date = new Date(Date.now() + 300_000); return new Date(date.getTime() - date.getTimezoneOffset() * 60_000).toISOString().slice(0, 16) })
  await page.getByLabel('发送时间（本地时区）').fill(localTime)
  await page.getByRole('button', { name: '创建定时任务', exact: true }).click()
  await expect(page.getByRole('dialog')).toBeHidden()
  await openList(); const drawer = page.getByRole('dialog')
  await expect(drawer.getByText('等待触发', { exact: true })).toBeVisible()
  await drawer.getByRole('button', { name: /^取\s*消$/ }).click(); expect(cancelled).toBe(false)
  await page.getByRole('button', { name: '确认取消', exact: true }).click()
  await expect(drawer.getByText('已取消', { exact: true })).toBeVisible()
})

test('a saved schedule from a closed dialog cannot close the next draft', async ({ page }) => {
  let release: () => void = () => {}, received = false
  const pending = new Promise<void>(resolve => { release = resolve })
  await page.route('**/api/v1/codex/sessions/ses-a/schedules', async route => { received = true; await pending; await route.fulfill({ json: { state: 'completed' } }) })
  await page.goto('/codex?session=ses-a')
  await expect(page.getByRole('heading', { name: '测试会话', exact: true })).toBeVisible()
  await page.getByRole('button', { name: '定时发送' }).click()
  const localTime = await page.evaluate(() => { const date = new Date(Date.now() + 300_000); return new Date(date.getTime() - date.getTimezoneOffset() * 60_000).toISOString().slice(0, 16) })
  await page.getByLabel('发送时间（本地时区）').fill(localTime)
  await page.getByLabel('指令', { exact: true }).fill('第一份调度')
  await page.getByRole('button', { name: '创建定时任务', exact: true }).click()
  await expect.poll(() => received).toBe(true)
  await page.getByRole('dialog').getByRole('button', { name: 'Close' }).click()
  await page.getByLabel('给 Codex 发送指令').fill('下一份调度草稿')
  await page.getByRole('button', { name: '定时发送' }).click()
  const saved = page.waitForResponse(response => response.url().endsWith('/ses-a/schedules'))
  release(); await saved; await page.waitForTimeout(100)
  await expect(page.getByRole('dialog')).toBeVisible()
  await expect(page.getByLabel('指令', { exact: true })).toHaveValue('下一份调度草稿')
})
