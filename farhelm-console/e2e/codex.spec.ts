import AxeBuilder from '@axe-core/playwright'
import { expect, test, type Page } from '@playwright/test'

const markdown = '## 训练结果\n\n**完整测量**，这里是合成验收内容。\n\n| 阶段 | 时间 |\n| --- | ---: |\n| 配准 | 132 ms |\n\n> 保留原始结论。\n\n- 首帧：0.8 秒\n- 后续：3–5 FPS\n\n[安全文档](https://example.org/paper)\n\n```python\nfor epoch in range(8):\n    train(epoch)\n```\n\n公式 $E=mc^2$，以及 \\(a^2+b^2=c^2\\)。\n\n\\[\\sum_{i=1}^{8} x_i\\]\n\n<script>window.injected=true</script>\n\n[危险链接](javascript:alert(1))\n\n'
const longText = markdown + ('这是一段用来验证长对话、中文阅读和独立滚动的合成回复。'.repeat(20) + '\n\n').repeat(8)
const session = (id = 'ses-a') => ({ session_id: id, agent_id: 'gpu-a', project_id: 'cc08', title: null as string | null, mode: 'inspect', state: 'idle', active_turn_id: undefined as string | undefined, updated_at_unix: 2000000000 })
const turn = (text = longText) => ({ turn_id: 'turn-a', status: 'completed', items: [{ item_id: 'user', kind: 'user_message', text: '请分析训练结果' }, { item_id: 'assistant', kind: 'assistant_message', text }, ...Array.from({ length: 12 }, (_, i) => ({ item_id: `tool-${i}`, kind: 'command_summary', text: `python … · exit ${i === 4 ? 1 : 0}`, status: i === 4 ? 'failed' : 'completed', duration_ms: 100, exit_code: i === 4 ? 1 : 0 }))] })
async function setup(page: Page) {
  const model = { sessions: [session(), session('ses-b')], history: { session_id: 'ses-a', turns: [turn()] }, historyStatus: 200, historyError: 'agent_read_failed', historyReads: 0 }
  await page.addInitScript(() => {
    const sources: EventTarget[] = []
    class Source extends EventTarget { constructor() { super(); sources.push(this) } close() { sources.splice(sources.indexOf(this), 1) } }
    Object.assign(window, { EventSource: Source, emitCodex: (type: string, payload: unknown) => sources.forEach((source) => source.dispatchEvent(new MessageEvent(type, { data: JSON.stringify({ payload }) }))) })
  })
  await page.route('**/api/v1/**', (route) => {
    const path = new URL(route.request().url()).pathname
    if (path.endsWith('/project-preferences')) return route.fulfill({ json: { revision: 0, projects: [] } })
    if (path.endsWith('/auth/session')) return route.fulfill({ json: { authenticated: true, user: 'admin', csrf_token: 'test-csrf', expires_at_unix: 2000000000 } })
    if (path.endsWith('/agents')) return route.fulfill({ json: { protocol: 'farhelm/1', agents: [{ agent_id: 'gpu-a', hostname: 'TITAN', agent_version: '0.8.0', online: true, last_seen_unix: 2000000000, capabilities: ['codex.session_context'] }] } })
    if (path.endsWith('/codex/sessions')) return route.fulfill({ json: { protocol: 'farhelm/1', sessions: model.sessions } })
    if (path.endsWith('/session-display')) return route.fulfill({ json: { protocol: 'farhelm/1', sessions: model.sessions.map((s) => ({ ...s, display_label: s.session_id === 'ses-a' ? '训练结果分析' : '另一个会话' })), incomplete_agents: [] } })
    if (path.endsWith('/transcript')) { model.historyReads++; return route.fulfill({ status: model.historyStatus, json: model.historyStatus !== 200 ? { error: model.historyError } : path.includes('ses-b') ? { session_id: 'ses-b', turns: [turn('另一会话的回复')] } : model.history }) }
    if (path.includes('/codex/sessions/')) return route.fulfill({ json: model.sessions.find((s) => path.endsWith(s.session_id)) ?? {} })
    if (path.endsWith('/projects')) return route.fulfill({ json: { protocol: 'farhelm/1', projects: [{ candidate_id: 'project-a', agent_id: 'gpu-a', suggested_project_id: 'cc08', display_name: 'cc08', state: 'approved', session_count: 2 }] } })
    if (path.endsWith('/experiments')) return route.fulfill({ json: { protocol: 'farhelm/1', experiments: [] } })
    if (path.includes('/schedules')) return route.fulfill({ json: { protocol: 'farhelm/1', schedules: [] } })
    if (path.endsWith('/notifications/preferences')) return route.fulfill({ json: { experiments: true, codex: true } })
    return route.fulfill({ json: { notifications: [], latest_id: 0, unread_count: 0 } })
  })
  return model
}
async function emit(page: Page, type: string, payload: unknown) { await page.evaluate(({ type, payload }) => (window as unknown as { emitCodex: (type: string, payload: unknown) => void }).emitCodex(type, payload), { type, payload }) }
async function choose(page: Page, name: string) { if (await page.getByRole('button', { name: '打开会话列表' }).isVisible()) await page.getByRole('button', { name: '打开会话列表' }).click(); await page.getByRole('button', { name: new RegExp(name) }).filter({ visible: true }).click() }
async function composerFits(page: Page) {
  const bounds = await page.locator('.composer-wrap').boundingBox(); const button = await page.getByRole('button', { name: '发送指令' }).boundingBox()
  expect(bounds).not.toBeNull(); expect(bounds!.y).toBeGreaterThanOrEqual(0); expect(bounds!.y + bounds!.height).toBeLessThanOrEqual(page.viewportSize()!.height + 1)
  expect(button!.y + button!.height).toBeLessThanOrEqual(page.viewportSize()!.height)
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false)
  const navigation = page.getByRole('navigation', { name: '主要导航' })
  if (await navigation.isVisible()) { const nav = await navigation.boundingBox(); expect(bounds!.y + bounds!.height).toBeLessThanOrEqual(nav!.y + 1) }
}

test('welcome leads to a real session and suggestions only prepare a focused draft', async ({ page }) => {
  const model = await setup(page)
  model.history.turns = []
  let sends = 0
  await page.route('**/ses-a/messages', (route) => { sends++; return route.fulfill({ json: { state: 'accepted' } }) })
  await page.goto('/codex')
  await expect(page.getByRole('heading', { name: '继续你的工作' })).toBeVisible()
  await expect(page.getByRole('button', { name: '开启新会话' })).toBeVisible()
  await expect(page.locator('.composer')).toHaveCount(0)
  await page.locator('.recent-session').filter({ hasText: '训练结果分析' }).click()
  await expect(page).toHaveURL(/session=ses-a/)
  await expect(page.getByRole('heading', { name: '准备好，开始下一步' })).toBeVisible()
  await page.getByRole('button', { name: '检查代码问题' }).click()
  const input = page.getByLabel('给 Codex 发送指令')
  await expect(input).toBeFocused()
  await expect(input).toHaveValue(/检查这个项目可能存在的问题/)
  expect(sends).toBe(0)
  await choose(page, '另一个会话')
  await choose(page, '训练结果分析')
  await expect(input).toHaveValue(/检查这个项目可能存在的问题/)
  await expect(page.getByRole('button', { name: '检查代码问题' })).toHaveCount(0)
  await composerFits(page)
})

test('projects group sessions across devices and collapse independently without changing targets', async ({ page }) => {
  const model = await setup(page)
  model.sessions.push({ ...session('ses-c'), agent_id: 'gpu-b', title: '另一台服务器的会话' })
  model.sessions.push({ ...session('ses-d'), project_id: 'vision', title: '另一个项目的会话' })
  await page.route('**/api/v1/projects', (route) => route.fulfill({ json: { protocol: 'farhelm/1', projects: [...['gpu-a', 'gpu-b'].map((agent_id, index) => ({ candidate_id: agent_id, agent_id, suggested_project_id: 'cc08', display_name: '共享项目', state: 'approved', last_activity_unix: 100 - index })), { candidate_id: 'vision', agent_id: 'gpu-a', suggested_project_id: 'vision', display_name: 'vision', state: 'approved' }] } }))
  await page.route('**/api/v1/agents', (route) => route.fulfill({ json: { protocol: 'farhelm/1', agents: ['gpu-a', 'gpu-b'].map((agent_id, index) => ({ agent_id, hostname: index ? '3090' : 'TITAN', agent_version: '0.8.0', last_seen_unix: 2000000000, online: !index, credential_state: 'paired' })) } }))
  await page.goto('/codex?session=ses-a')
  await expect(page.locator('.conversation-title')).toContainText('训练结果分析')
  await page.getByLabel('给 Codex 发送指令').fill('折叠项目时保留草稿')
  if (await page.getByRole('button', { name: '打开会话列表' }).isVisible()) {
    await page.getByRole('button', { name: '打开会话列表' }).click()
    await expect(page.getByRole('dialog', { name: '项目和会话' })).toBeVisible()
  }
  const rail = page.getByRole('complementary', { name: '项目和会话' }).filter({ visible: true })
  const headers = rail.locator('.session-group-heading')
  await expect(headers).toHaveCount(3)
  await expect(headers.nth(0)).toHaveAccessibleName('项目 共享项目 · TITAN')
  await expect(headers.nth(1)).toHaveAccessibleName('项目 共享项目 · 3090')
  await expect(headers.nth(2)).toHaveAccessibleName('项目 vision · TITAN')
  await expect(headers.nth(1)).toHaveAccessibleDescription('设备离线')
  const first = rail.getByRole('button', { name: '项目 共享项目 · TITAN', exact: true })
  const other = rail.getByRole('button', { name: '项目 共享项目 · 3090', exact: true })
  const nameBounds = await first.locator('.project-name').boundingBox()
  const deviceBounds = await first.locator('.project-device').boundingBox()
  expect(Math.abs(nameBounds!.y - deviceBounds!.y)).toBeLessThan(3)
  expect(deviceBounds!.x).toBeGreaterThan(nameBounds!.x)
  expect((await first.boundingBox())!.height).toBeGreaterThanOrEqual(44)
  await first.focus(); await page.keyboard.press('Enter')
  await expect(first).toHaveAttribute('aria-expanded', 'false')
  await expect(rail.getByRole('button', { name: '训练结果分析', exact: true })).toBeHidden()
  await expect(other).toHaveAttribute('aria-expanded', 'true')
  await expect(page).toHaveURL(/session=ses-a/)
  const remote = rail.getByRole('button', { name: '另一台服务器的会话' })
  await remote.focus(); await page.keyboard.press('Home')
  await expect(remote).toBeFocused()
  await page.keyboard.press('ArrowUp')
  await expect(remote).toBeFocused()
  await page.keyboard.press('Enter')
  await expect(page).toHaveURL(/session=ses-c/)
  if (await page.getByRole('button', { name: '打开会话列表' }).isVisible()) await page.getByRole('button', { name: '打开会话列表' }).click()
  await expect(first).toHaveAttribute('aria-expanded', 'false')
  await first.click()
  await rail.getByRole('button', { name: '训练结果分析', exact: true }).click()
  await expect(page.getByLabel('给 Codex 发送指令')).toHaveValue('折叠项目时保留草稿')
  await composerFits(page)
})

test('search shortcut opens the collapsed rail without changing the active draft', async ({ page }, info) => {
  test.skip(info.project.name === 'mobile', 'Desktop keyboard shortcut')
  await setup(page); await page.goto('/codex?session=ses-a')
  await page.getByLabel('给 Codex 发送指令').fill('未发送的草稿')
  await page.getByRole('button', { name: '折叠会话列表' }).click()
  await page.keyboard.press('Control+k')
  await expect(page.getByLabel('搜索全部会话')).toBeFocused()
  await expect(page.getByLabel('给 Codex 发送指令')).toHaveValue('未发送的草稿')
  await expect(page.locator('.app-sider')).toBeVisible()
})

test('compact navigation preserves every destination and aligns the reading column', async ({ page }, info) => {
  test.skip(info.project.name === 'mobile', 'Desktop navigation and reading alignment')
  await setup(page); await page.goto('/codex?session=ses-a')
  await expect(page.locator('.markdown-body').first()).toBeVisible()
  const nav = page.getByRole('navigation', { name: '系统导航' })
  for (const name of ['总览', 'Agent', '实验', 'Codex', '通知', '审计', '设置']) await expect(nav.getByRole('menuitem', { name, exact: true })).toBeVisible()
  expect((await page.locator('.app-sider').boundingBox())!.width).toBe(88)
  expect((await page.locator('.codex-desktop-rail').boundingBox())!.width).toBe(300)
  const heading = await page.locator('.conversation-title h1').boundingBox()
  const body = await page.locator('.codex-transcript').boundingBox()
  expect(Math.abs(heading!.x - body!.x)).toBeLessThan(2)
  expect(await page.evaluate(() => getComputedStyle(document.body).fontFamily)).toContain('Noto Sans SC Variable')
  await composerFits(page)
})

test('project scope popover closes after selection and preserves the active draft', async ({ page }) => {
  await setup(page)
  await page.route('**/api/v1/projects', (route) => route.fulfill({ json: { protocol: 'farhelm/1', projects: [{ candidate_id: 'project-a', agent_id: 'gpu-a', suggested_project_id: 'cc08', display_name: '训练项目', state: 'approved' }] } }))
  await page.goto('/codex?session=ses-a')
  await page.getByLabel('给 Codex 发送指令').fill('筛选期间保留这条草稿')
  if (await page.getByRole('button', { name: '打开会话列表' }).isVisible()) await page.getByRole('button', { name: '打开会话列表' }).click()
  const rail = page.getByRole('complementary', { name: '项目和会话' }).filter({ visible: true })
  await rail.getByRole('button', { name: '筛选服务器与项目' }).click()
  await page.getByLabel('筛选项目').click()
  const scoped = page.waitForRequest((request) => new URL(request.url()).pathname.endsWith('/codex/sessions') && new URL(request.url()).searchParams.get('agent') === 'gpu-a' && new URL(request.url()).searchParams.get('project') === 'cc08')
  await page.getByText('gpu-a / 训练项目', { exact: true }).click()
  await scoped
  await expect(rail.locator('.active-session-scope')).toContainText('gpu-a / 训练项目')
  await expect(page.getByLabel('筛选项目')).toBeHidden()
  await rail.getByRole('button', { name: '清除项目筛选' }).click()
  await expect(rail.locator('.active-session-scope')).toHaveCount(0)
  await rail.getByRole('button', { name: '训练结果分析', exact: true }).click()
  await expect(page.getByLabel('给 Codex 发送指令')).toHaveValue('筛选期间保留这条草稿')
  await composerFits(page)
})

test('short desktop windows keep system navigation reachable above the account footer', async ({ page }, info) => {
  test.skip(info.project.name === 'mobile', 'Desktop navigation geometry')
  await setup(page); await page.setViewportSize({ width: 1440, height: 500 })
  await page.goto('/codex?session=ses-a')
  const settings = page.getByRole('navigation', { name: '系统导航' }).getByRole('menuitem', { name: /设置/ })
  await settings.scrollIntoViewIfNeeded()
  const item = await settings.boundingBox(), footer = await page.locator('.sider-footer').boundingBox()
  expect(item!.y).toBeGreaterThanOrEqual(0)
  expect(item!.y + item!.height).toBeLessThanOrEqual(footer!.y)
  await settings.click()
  await expect(page.getByRole('heading', { name: '设置', exact: true })).toBeVisible()
})

test('connection status follows the shared stream and keeps the draft during reconnect', async ({ page }) => {
  await setup(page); await page.goto('/codex?session=ses-a')
  await page.getByLabel('给 Codex 发送指令').fill('断线后继续编辑')
  await emit(page, 'open', {})
  await expect(page.getByRole('status', { name: '实时连接' })).toBeVisible()
  await emit(page, 'error', {})
  await expect(page.getByRole('status', { name: '正在重连' })).toBeVisible()
  await expect(page.getByLabel('给 Codex 发送指令')).toHaveValue('断线后继续编辑')
  await emit(page, 'open', {})
  await expect(page.getByRole('status', { name: '实时连接' })).toBeVisible()
})

test('motion communicates work and submission while reduced motion keeps the same controls', async ({ page }) => {
  const model = await setup(page)
  model.sessions[0].active_turn_id = 'turn-a'; model.sessions[0].state = 'running'
  await page.emulateMedia({ reducedMotion: 'no-preference' })
  await page.goto('/codex?session=ses-a')
  await expect(page.locator('.response-activity')).toContainText('Codex 正在处理')
  expect(await page.locator('.activity-indicator i').first().evaluate((node) => node.getAnimations().some((animation) => animation.playState === 'running'))).toBe(true)
  await expect(page.locator('.markdown-table')).toHaveCount(1)
  await expect(page.locator('.response-activity')).toBeInViewport()
  await page.locator('.conversation-scroll').hover()
  await page.mouse.wheel(0, -720)
  await expect(page.getByRole('button', { name: '回到底部', exact: true })).toBeVisible()
  const input = page.getByLabel('给 Codex 发送指令')
  await input.fill('下一轮继续检查')
  await expect.poll(() => page.locator('.composer').evaluate((node) => getComputedStyle(node, '::after').opacity)).toBe('1')
  let release: () => void = () => {}
  const gate = new Promise<void>((resolve) => { release = resolve })
  await page.route('**/ses-a/messages', async (route) => { await gate; return route.fulfill({ json: { command_id: 'cmd-motion', state: 'accepted' } }) })
  await page.getByRole('button', { name: '发送指令' }).click()
  await expect(input).toBeFocused()
  await expect(page.locator('.pending-message')).toContainText('正在提交')
  await expect(page.locator('.pending-message')).toBeInViewport()
  expect(await page.locator('.pending-message').evaluate((node) => getComputedStyle(node).animationName)).toBe('message-submit')
  expect(await page.locator('.codex-message.assistant').first().evaluate((node) => node.getAnimations().length)).toBe(0)
  await input.fill('保存确认到达前，继续编写新的草稿')
  release()
  await expect(page.locator('.submission-status')).toContainText('Agent 已保存')
  await expect(input).toHaveValue('保存确认到达前，继续编写新的草稿')
  await choose(page, '另一个会话'); await choose(page, '训练结果分析')
  await expect(input).toHaveValue('保存确认到达前，继续编写新的草稿')
  expect(await page.locator('.composer').evaluate((node) => getComputedStyle(node).animationName)).toBe('none')
  expect(await page.locator('.pending-message').evaluate((node) => getComputedStyle(node).animationName)).toBe('none')
  await page.emulateMedia({ reducedMotion: 'reduce' })
  await expect.poll(() => page.locator('.activity-indicator i').first().evaluate((node) => node.getAnimations().length)).toBe(0)
  await input.fill('减少动态效果时继续输入')
  const sendButton = page.getByRole('button', { name: '发送指令' })
  await sendButton.hover(); await page.mouse.down()
  expect(await sendButton.evaluate((node) => getComputedStyle(node).transform)).toBe('none')
  await page.mouse.move(1, 1); await page.mouse.up()
  await expect(page.locator('.response-activity')).toContainText('Codex 正在处理')
  await expect(page.getByRole('button', { name: '中断', exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: '定时发送', exact: true })).toBeVisible()
  await composerFits(page)
})

test('interrupt confirmation inherits the page theme and still requires confirmation of the visible turn', async ({ page }) => {
  const model = await setup(page)
  model.sessions[0].state = 'running'; model.sessions[0].active_turn_id = 'turn-a'
  const targets: unknown[] = []
  await page.route('**/ses-a/interrupt', route => { targets.push(route.request().postDataJSON()); return route.fulfill({ json: { state: 'completed' } }) })
  for (const colorScheme of ['light', 'dark'] as const) {
    await page.emulateMedia({ colorScheme }); await page.goto('/codex?session=ses-a')
    await page.getByRole('button', { name: '中断', exact: true }).click()
    const dialog = page.getByRole('dialog', { name: '中断当前对话？' })
    await expect(dialog.locator('.ant-modal-container')).toHaveCSS('background-color', colorScheme === 'dark' ? 'rgb(48, 48, 48)' : 'rgb(240, 240, 240)')
    await expect(dialog).toContainText('轮次 turn-a')
    await dialog.getByRole('button', { name: /^取\s*消$/ }).click()
    expect(targets).toHaveLength(colorScheme === 'light' ? 0 : 1)
    await expect(dialog).toBeHidden()
    await page.getByRole('button', { name: '中断', exact: true }).click()
    await dialog.getByRole('button', { name: '确认中断', exact: true }).click()
    await expect(dialog).toBeHidden()
    expect(targets.at(-1)).toEqual({ turn_id: 'turn-a' })
  }
})

for (const theme of ['light', 'dark'] as const) for (const size of [{ width: 390, height: 844 }, { width: 1440, height: 900 }, { width: 2550, height: 1233 }]) {
  test(`Markdown and fixed workspace ${size.width} ${theme}`, async ({ page }, info) => {
    test.skip(info.project.name !== 'desktop', 'Explicit viewport matrix runs once')
    await page.setViewportSize(size); await page.emulateMedia({ colorScheme: theme, reducedMotion: 'reduce' }); await setup(page)
    await page.goto('/codex?session=ses-a')
    await expect(page.locator('.conversation-title')).toContainText('训练结果分析')
    if (size.width >= 768) { await expect(page.locator('.app-sider')).toBeVisible(); await expect(page.locator('.app-sider .ant-menu-item-selected')).toContainText('Codex') }
    else await expect(page.getByRole('navigation', { name: '主要导航' })).toBeVisible()
    await expect(page.getByRole('heading', { name: '最新对话' })).toHaveCount(1)
    await expect(page.locator('.markdown-table table')).toHaveCount(1)
    await expect(page.locator('.markdown-body strong')).toContainText('完整测量')
    await expect(page.locator('.katex')).toHaveCount(3)
    await expect(page.locator('.execution-summary')).toHaveCount(1)
    await expect(page.getByText('含失败')).toBeVisible()
    await composerFits(page)
    await expect(page.locator('a[href^="javascript:"]')).toHaveCount(0)
    expect(await page.evaluate(() => (window as unknown as { injected?: boolean }).injected)).toBeUndefined()
    await expect(page.getByRole('link', { name: '安全文档' })).toHaveAttribute('href', 'https://example.org/paper')
    await page.locator('.conversation-scroll').evaluate((node) => { const heading = node.querySelector('h3')!; node.scrollTop += heading.getBoundingClientRect().top - node.getBoundingClientRect().top - 20 })
    await page.screenshot({ path: info.outputPath(`codex-${size.width}-${theme}.png`) })
    const results = await new AxeBuilder({ page }).analyze()
    expect(results.violations.filter((v) => ['critical', 'serious'].includes(v.impact ?? ''))).toEqual([])
    await page.getByRole('button', { name: '复制代码' }).scrollIntoViewIfNeeded()
    await expect(page.locator('.markdown-code code')).toHaveClass(/language-python/)
    await page.locator('.execution-summary').getByRole('button').click()
    await expect(page.locator('.tool-items li')).toHaveCount(12)
    await composerFits(page)
  })
}

test('composer survives errors, sidebar collapse and keyboard-sized viewport', async ({ page }) => {
  await setup(page); await page.goto('/codex?session=ses-a'); await expect(page.locator('.markdown-table')).toHaveCount(1)
  await page.route('**/ses-a/messages', (route) => route.fulfill({ status: 504, json: { error: 'agent_save_unconfirmed' } }))
  await page.getByLabel('给 Codex 发送指令').fill('保留这份草稿'); await page.getByRole('button', { name: '发送指令' }).click()
  await expect(page.getByText(/尚未确认 Agent 保存/)).toBeVisible(); await composerFits(page)
  if (await page.getByRole('button', { name: '折叠会话列表' }).isVisible()) { await page.getByRole('button', { name: '折叠会话列表' }).click(); await composerFits(page) }
  await page.setViewportSize({ width: 390, height: 500 }); await composerFits(page)
})

test('search reaches unloaded sessions and reports offline Agents', async ({ page }) => {
  await setup(page)
  let search: unknown
  await page.route('**/session-display', (route) => {
    const body = route.request().postDataJSON(); if (body.mode === 'search') search = body
    return route.fulfill({ json: { sessions: body.mode === 'search' ? [{ ...session('ses-999'), display_label: '第 999 个训练报告' }] : [], incomplete_agents: body.mode === 'search' ? [{ agent_id: 'gpu-b', reason: 'agent_offline' }] : [] } })
  })
  await page.goto('/codex?session=ses-a')
  await expect(page.locator('.conversation-title')).toContainText('cc08')
  if (await page.getByRole('button', { name: '打开会话列表' }).isVisible()) await page.getByRole('button', { name: '打开会话列表' }).click()
  await page.getByLabel('搜索全部会话').filter({ visible: true }).fill('训练报告')
  await expect(page.getByRole('button', { name: /第 999 个训练报告/ }).filter({ visible: true })).toBeVisible()
  expect(search).toMatchObject({ mode: 'search', query: '训练报告', archived: 'false' })
  await expect(page.getByText(/gpu-b（离线）/).filter({ visible: true })).toBeVisible()
})

test('keyboard viewport keeps a multiline draft, errors and active turn controls above navigation', async ({ page }) => {
  const model = await setup(page); model.sessions[0].active_turn_id = 'turn-a'; model.sessions[0].state = 'running'
  await page.goto('/codex?session=ses-a'); await expect(page.getByText('补充当前对话', { exact: true })).toBeVisible()
  await page.route('**/ses-a/messages', (route) => route.fulfill({ status: 504, json: { error: 'agent_save_unconfirmed' } }))
  await page.getByLabel('给 Codex 发送指令').fill('多行草稿\n'.repeat(15)); await page.getByRole('button', { name: '发送指令' }).click()
  await expect(page.getByText(/尚未确认 Agent 保存/)).toBeVisible()
  await page.setViewportSize({ width: 390, height: 440 })
  await expect(page.getByRole('navigation', { name: '主要导航' })).toBeVisible()
  await test.info().attach('keyboard-geometry', { contentType: 'application/json', body: JSON.stringify(await page.evaluate(() => Object.fromEntries(['.app-layout', '.app-header', '.codex-workspace', '.conversation-head', '.conversation-alerts', '.conversation-history', '.composer-wrap', '.composer', '.composer textarea', '.delivery-options', '.composer-actions', '.composer-hint', '.mobile-nav'].map((selector) => { const node = document.querySelector(selector)!; const box = node.getBoundingClientRect(); const style = getComputedStyle(node); return [selector, { y: box.y, height: box.height, minHeight: style.minHeight, maxHeight: style.maxHeight }] }))), null, 2) })
  await composerFits(page)
  await expect(page.getByRole('button', { name: '中断', exact: true })).toBeVisible()
})

test('history recovers when Agent heartbeat follows Hub restart', async ({ page }) => {
  const model = await setup(page); model.historyStatus = 503; model.historyError = 'agent_offline'
  await page.goto('/codex?session=ses-a')
  await expect(page.getByText('Agent 离线，暂时无法读取历史')).toBeVisible()
  model.historyStatus = 200
  await emit(page, 'agent.status', { agent_id: 'gpu-a', hostname: 'gpu-a', online: true, codex: { state: 'ready' } })
  await expect(page.locator('.markdown-table')).toHaveCount(1, { timeout: 10000 })
  await expect(page.getByText('Agent 离线，暂时无法读取历史')).toHaveCount(0)
  expect(model.historyReads).toBeLessThanOrEqual(4)
})

test('session metadata arriving after its creation receipt retries initial history', async ({ page }) => {
  const model = await setup(page); model.historyStatus = 404; model.historyError = 'session_not_found'
  await page.goto('/codex?session=ses-a')
  await expect(page.getByText('会话不存在或尚未导入')).toBeVisible()
  model.historyStatus = 200
  await emit(page, 'codex.session.updated', { session_id: 'ses-a' })
  await expect(page.locator('.markdown-table')).toHaveCount(1)
  await expect(page.getByLabel('给 Codex 发送指令')).toBeEnabled()
})

for (const cancelled of [false, true]) test(`created session follows its receipt; dialog cancelled=${cancelled}`, async ({ page }) => {
  const model = await setup(page); let ready = false; let reads = 0
  await page.route('**/projects', (route) => route.fulfill({ json: { protocol: 'farhelm/1', projects: [{ candidate_id: 'project-a', agent_id: 'gpu-a', suggested_project_id: 'cc08', display_name: '训练项目', state: 'approved' }] } }))
  await page.route('**/codex/sessions', (route) => route.request().method() === 'POST' ? route.fulfill({ json: { command_id: 'create-receipt', state: 'accepted' } }) : route.fallback())
  await page.route('**/commands/create-receipt', (route) => { reads++; return route.fulfill({ json: { command_id: 'create-receipt', state: ready ? 'completed' : 'running', data: ready ? { session_id: 'ses-created' } : undefined } }) })
  await page.route('**/ses-created/transcript?*', (route) => route.fulfill({ json: { session_id: 'ses-created', turns: [] } }))
  await page.goto('/codex?session=ses-a'); await expect(page.locator('.conversation-title')).toContainText('训练结果分析')
  await page.getByLabel('给 Codex 发送指令').fill('原会话草稿')
  if (await page.getByRole('button', { name: '打开会话列表' }).isVisible()) await page.getByRole('button', { name: '打开会话列表' }).click()
  await page.getByRole('button', { name: '新建会话' }).filter({ visible: true }).click()
  await page.getByLabel('项目', { exact: true }).click(); await page.getByLabel('项目', { exact: true }).press('Enter')
  await page.getByRole('button', { name: /创\s*建/, exact: true }).click(); await expect.poll(() => reads).toBeGreaterThan(0)
  if (cancelled) { const dialog = page.getByRole('dialog', { name: '创建 Codex 会话' }); await dialog.getByRole('button', { name: 'Close', exact: true }).press('Enter'); await expect(dialog).toBeHidden() }
  model.sessions.push({ ...session('ses-created'), title: '新建验收会话' }); ready = true
  await emit(page, 'command.updated', { command_id: 'create-receipt', state: 'completed', data: { session_id: 'ses-created' } })
  expect(reads).toBe(1)
  if (cancelled) { await expect(page).toHaveURL(/session=ses-a/); await expect(page.getByLabel('给 Codex 发送指令')).toHaveValue('原会话草稿') }
  else { await expect(page).toHaveURL(/session=ses-created/); await expect(page.locator('.conversation-title')).toContainText('新建验收会话'); await expect(page.getByLabel('给 Codex 发送指令')).toHaveValue('') }
})

test('failed send belongs to its original session while switching', async ({ page }) => {
  await setup(page); let release: () => void = () => {}; const gate = new Promise<void>((r) => { release = r })
  await page.route('**/ses-a/messages', async (route) => { await gate; await route.fulfill({ status: 504, json: { error: 'agent_save_unconfirmed' } }) })
  await page.goto('/codex?session=ses-a'); await expect(page.locator('.conversation-title')).toContainText('训练结果分析')
  await page.getByLabel('给 Codex 发送指令').fill('A 会话的草稿'); await page.getByRole('button', { name: '发送指令' }).click()
  await expect(page.getByText('你 · 正在提交')).toBeVisible(); await choose(page, '另一个会话'); await expect(page).toHaveURL(/session=ses-b/)
  await page.getByLabel('给 Codex 发送指令').fill('B 的草稿'); release()
  await expect(page.getByText(/尚未确认 Agent 保存/)).toHaveCount(0); await expect(page.getByLabel('给 Codex 发送指令')).toHaveValue('B 的草稿')
  await choose(page, '训练结果分析'); await expect(page.getByLabel('给 Codex 发送指令')).toHaveValue('A 会话的草稿'); await expect(page.getByText(/尚未确认 Agent 保存/)).toBeVisible()
  await page.goBack(); await expect(page.getByLabel('给 Codex 发送指令')).toHaveValue('B 的草稿')
})

test('interleaved and repeated deltas retain item identity after a failed terminal refresh', async ({ page }) => {
  const model = await setup(page); model.history = { session_id: 'ses-a', turns: [] }
  await page.goto('/codex?session=ses-a'); await expect(page.getByRole('heading', { name: '准备好，开始下一步' })).toBeVisible()
  const delta = async (item_id: string, text_offset: number, delta: string) => emit(page, 'codex.message.delta', { session_id: 'ses-a', data: { turn_id: 'stream-turn', item_id, text_offset, delta } })
  await delta('a', 0, '第一条🙂'); await delta('b', 0, '第二条'); await delta('a', 4, '完成'); await delta('a', 4, '完成')
  await expect(page.locator('.codex-message.assistant')).toHaveCount(2)
  await expect(page.locator('.codex-message.assistant').first()).toContainText('第一条🙂完成')
  model.historyStatus = 502
  await emit(page, 'codex.turn.completed', { session_id: 'ses-a', data: { turn_id: 'stream-turn' } })
  await expect(page.getByText('历史刷新失败，已保留当前内容')).toBeVisible(); await expect(page.getByText('第二条', { exact: true })).toBeVisible()
  model.historyStatus = 200; model.history = { session_id: 'ses-a', turns: [{ turn_id: 'stream-turn', status: 'completed', items: [{ item_id: 'a', kind: 'assistant_message', text: '第一条🙂完成' }, { item_id: 'b', kind: 'assistant_message', text: '第二条' }] }] }
  await page.getByRole('button', { name: '刷新对话' }).click(); await expect(page.getByText('历史刷新失败，已保留当前内容')).toHaveCount(0)
  await delta('a', 4, '完成'); await expect(page.locator('.codex-message.assistant')).toHaveCount(2)
})

test('reading above the bottom preserves the anchor when new messages arrive', async ({ page }) => {
  await setup(page); await page.goto('/codex?session=ses-a'); await expect(page.locator('.markdown-table')).toHaveCount(1)
  const scroll = page.locator('.conversation-scroll'); await scroll.evaluate((node) => { node.scrollTop = 400 })
  await expect.poll(() => scroll.evaluate((node) => node.scrollTop)).toBe(400)
  await emit(page, 'codex.message.delta', { session_id: 'ses-a', data: { turn_id: 'new-turn', item_id: 'new-item', text_offset: 0, delta: '新的回复内容' } })
  await expect(page.getByRole('button', { name: /有新消息/ })).toBeVisible(); expect(await scroll.evaluate((node) => node.scrollTop)).toBeCloseTo(400, 0)
  await page.getByRole('button', { name: /有新消息/ }).click(); await expect(page.getByText('新的回复内容')).toBeVisible()
  await expect(page.getByRole('button', { name: /有新消息/ })).toBeHidden()
})

test('one MiB message resumes separately from earlier turns without duplicate text', async ({ page }) => {
  await setup(page)
  const text = '训练🙂结果\n'.repeat(70000)
  const pieces = Array.from(text); const offsets = [0, 140000, 280000, pieces.length]
  await page.route('**/ses-a/transcript?*', (route) => {
    const index = Number(new URL(route.request().url()).searchParams.get('cursor') ?? '0'); const start = offsets[index]; const end = offsets[index + 1]
    return route.fulfill({ json: { session_id: 'ses-a', turns: [{ turn_id: 'large', status: 'completed', items: [{ item_id: 'large-message', kind: 'assistant_message', text: pieces.slice(start, end).join(''), text_offset: start, text_complete: index === 2 }] }], next_cursor: index < 2 ? String(index + 1) : null, continuation: index < 2 ? { kind: 'message', turn_id: 'large', item_id: 'large-message', text_offset: end } : null } })
  })
  await page.goto('/codex?session=ses-a')
  await expect(page.getByRole('button', { name: '继续加载此消息' })).toBeVisible()
  await expect(page.getByRole('button', { name: '加载更早对话' })).toHaveCount(0)
  await page.getByLabel('给 Codex 发送指令').fill('大消息解析期间仍可输入')
  await page.getByRole('button', { name: '继续加载此消息' }).click()
  await expect(page.getByRole('button', { name: '继续加载此消息' })).toBeEnabled()
  await page.getByRole('button', { name: '继续加载此消息' }).click()
  await expect(page.getByRole('button', { name: '继续加载此消息' })).toHaveCount(0)
  await expect(page.locator('.markdown-body [aria-busy="false"]')).toHaveCount(1, { timeout: 20000 })
  await expect(page.locator('.markdown-fallback')).toHaveCount(0)
  expect(await page.locator('.long-paragraph-chunk').count()).toBeGreaterThan(0)
  // Markdown consumes the closing paragraph newline; every content character
  // and interior line break must remain across all three transport fragments.
  expect(await page.locator('.markdown-body').textContent()).toBe(text.trimEnd())
  await page.getByRole('button', { name: '刷新对话' }).click()
  await expect(page.getByRole('button', { name: '继续加载此消息' })).toHaveCount(0)
  expect(await page.locator('.markdown-body').textContent()).toBe(text.trimEnd())
  await expect(page.getByLabel('给 Codex 发送指令')).toHaveValue('大消息解析期间仍可输入'); await composerFits(page)
})

test('2000 turns load through every page and retain the visible anchor when virtualization starts', async ({ page }, info) => {
  test.skip(info.project.name !== 'desktop', 'Long-history acceptance runs once')
  test.setTimeout(90000)
  // Delayed layout/measurement must not move the reader when rows switch from
  // ordinary DOM to estimated virtual heights, or when another page is prepended.
  const cdp = await page.context().newCDPSession(page)
  await cdp.send('Emulation.setCPUThrottlingRate', { rate: 4 })
  await setup(page)
  const cursors: number[] = []
  await page.route('**/ses-a/transcript*', (route) => {
    const offset = Number(new URL(route.request().url()).searchParams.get('cursor') ?? 0); cursors.push(offset)
    return route.fulfill({ json: { session_id: 'ses-a', turns: Array.from({ length: 20 }, (_, index) => {
      const number = 1999 - offset - index
      return { turn_id: `long-${number}`, status: 'completed', items: [{ item_id: 'user', kind: 'user_message', text: `合成问题 ${number}` }, { item_id: 'assistant', kind: 'assistant_message', text: `合成回复 ${number}` }] }
    }), next_cursor: offset < 1980 ? String(offset + 20) : null } })
  })
  await page.goto('/codex?session=ses-a')
  await expect(page.locator('[data-turn-id="long-1999"]')).toBeVisible()
  for (let index = 1; index < 100; index++) {
    const button = page.getByRole('button', { name: '加载更早对话' })
    await button.scrollIntoViewIfNeeded()
    if (index <= 3) await expect.poll(() => page.locator('.conversation-scroll').evaluate(node => {
      const bounds = node.getBoundingClientRect()
      return [...node.querySelectorAll('[data-message-key]')].some(item => { const rect = item.getBoundingClientRect(); return rect.bottom > bounds.top && rect.top < bounds.bottom })
    })).toBe(true)
    const previous = await page.locator('.conversation-scroll').evaluate((node) => {
      const top = node.getBoundingClientRect().top
      const first = [...node.querySelectorAll<HTMLElement>('[data-message-key]')].find((item) => item.getBoundingClientRect().bottom > top)
      return first && { key: first.dataset.messageKey, offset: first.getBoundingClientRect().top - top }
    })
    await button.click()
    await expect.poll(() => cursors.length).toBe(index + 1)
    await expect(page.locator('.conversation-scroll [aria-busy="true"]')).toHaveCount(0)
    if (index < 99) await expect(button).toBeEnabled()
    if (index <= 3 && previous) await expect.poll(async () => {
      return page.locator('.conversation-scroll').evaluate(async (node, old) => {
        let largest = 0
        // Check sustained position, including delayed ResizeObserver frames.
        for (let frame = 0; frame < 6; frame++) {
          await new Promise(requestAnimationFrame)
          const item = node.querySelector<HTMLElement>(`[data-message-key="${CSS.escape(old.key!)}"]`)
          largest = Math.max(largest, item ? Math.abs(item.getBoundingClientRect().top - node.getBoundingClientRect().top - old.offset) : 99999)
        }
        return largest
      }, previous)
    }, { message: `Reading anchor after page ${index + 1}` }).toBeLessThan(5)
    if (index === 3) await cdp.send('Emulation.setCPUThrottlingRate', { rate: 1 })
  }
  expect(new Set(cursors).size).toBe(100)
  await expect(page.getByRole('button', { name: '加载更早对话' })).toHaveCount(0)
  await page.locator('.conversation-scroll').evaluate((node) => { node.scrollTop = 0 })
  await expect(page.locator('[data-turn-id="long-0"]')).toBeVisible()
  expect(await page.locator('.codex-turn').count()).toBeLessThan(35)
  await page.getByLabel('给 Codex 发送指令').fill('长历史中仍可输入中文🙂')
  await page.getByRole('button', { name: /回到底部/ }).click()
  await expect(page.locator('[data-turn-id="long-1999"]')).toBeVisible()
  await composerFits(page)
  await choose(page, '另一个会话')
  await expect(page.getByText('另一会话的回复', { exact: true })).toBeVisible()
  let resume: () => void = () => {}
  const waiting = new Promise<void>(resolve => { resume = resolve })
  // A cached virtual transcript must attach without a metadata/history response triggering another render.
  await page.route('**/api/v1/**', async route => { await waiting; await route.fallback().catch(() => {}) })
  try {
    await choose(page, '训练结果分析')
    await expect(page.locator('[data-turn-id="long-1999"]')).toBeVisible()
    await expect(page.getByLabel('给 Codex 发送指令')).toHaveValue('长历史中仍可输入中文🙂')
    expect(await page.locator('.codex-turn').count()).toBeLessThan(35)
  } finally { resume() }
})

test('Enter submits identical prompts with independent identities and no command polling', async ({ page }, info) => {
  test.skip(info.project.name !== 'desktop', 'Submission acceptance runs once')
  await setup(page)
  const identities: string[] = []; let statusReads = 0
  await page.route('**/commands/*', (route) => { statusReads++; return route.fulfill({ json: { state: 'completed' } }) })
  await page.route('**/ses-a/messages', (route) => {
    identities.push(route.request().headers()['idempotency-key'])
    return route.fulfill({ json: { command_id: `enter-${identities.length}`, state: 'accepted' } })
  })
  await page.goto('/codex?session=ses-a')
  const input = page.getByLabel('给 Codex 发送指令')
  for (let index = 1; index <= 20; index++) {
    await input.fill('相同的中文指令'); await input.press('Enter'); await expect(input).toHaveValue('')
    await emit(page, 'command.updated', { command_id: `enter-${index}`, state: 'completed' })
  }
  expect(new Set(identities).size).toBe(20); expect(statusReads).toBe(0)
})

test('separated terminal events and receipts reconcile each execution once in either order', async ({ page }, info) => {
  test.skip(info.project.name !== 'desktop', 'Transport reconciliation runs once')
  const model = await setup(page)
  await page.goto('/codex?session=ses-a')
  await expect(page.locator('.markdown-body strong')).toContainText('完整测量')
  for (const receiptFirst of [false, true]) {
    const operation = `dedupe-${receiptFirst}`; const turnId = `turn-${operation}`
    const before = model.historyReads
    const terminal = () => emit(page, 'codex.turn.completed', { session_id: 'ses-a', operation_id: operation, data: { turn_id: turnId } })
    const receipt = () => emit(page, 'command.updated', { command_id: operation, state: 'completed', data: { session_id: 'ses-a', turn_id: turnId } })
    await (receiptFirst ? receipt() : terminal())
    await expect.poll(() => model.historyReads).toBe(before + 1)
    // The second event arrives after the first read, beyond the coalescing window.
    await (receiptFirst ? terminal() : receipt())
    await terminal(); await receipt()
    await page.waitForTimeout(180)
    expect(model.historyReads).toBe(before + 1)
  }
})
