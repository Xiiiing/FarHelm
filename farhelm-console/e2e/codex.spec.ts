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
    if (path.endsWith('/auth/session')) return route.fulfill({ json: { authenticated: true, user: 'admin', csrf_token: 'test-csrf', expires_at_unix: 2000000000 } })
    if (path.endsWith('/codex/sessions')) return route.fulfill({ json: { protocol: 'farhelm/1', sessions: model.sessions } })
    if (path.endsWith('/session-display')) return route.fulfill({ json: { protocol: 'farhelm/1', sessions: model.sessions.map((s) => ({ ...s, display_label: s.session_id === 'ses-a' ? '训练结果分析' : '另一个会话' })), incomplete_agents: [] } })
    if (path.endsWith('/transcript')) { model.historyReads++; return route.fulfill({ status: model.historyStatus, json: model.historyStatus !== 200 ? { error: model.historyError } : path.includes('ses-b') ? { session_id: 'ses-b', turns: [turn('另一会话的回复')] } : model.history }) }
    if (path.includes('/codex/sessions/')) return route.fulfill({ json: model.sessions.find((s) => path.endsWith(s.session_id)) ?? {} })
    if (path.endsWith('/projects')) return route.fulfill({ json: { protocol: 'farhelm/1', projects: [] } })
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
  await page.goto('/codex?session=ses-a'); await expect(page.getByText(/这个会话还没有对话/)).toBeVisible()
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
    const previous = await page.locator('.conversation-scroll').evaluate((node) => {
      const top = node.getBoundingClientRect().top
      const first = [...node.querySelectorAll<HTMLElement>('[data-message-key]')].find((item) => item.getBoundingClientRect().bottom > top)
      return first && { key: first.dataset.messageKey, offset: first.getBoundingClientRect().top - top }
    })
    await button.click()
    await expect.poll(() => cursors.length).toBe(index + 1)
    await expect(page.locator('.conversation-scroll [aria-busy="true"]')).toHaveCount(0)
    if (index < 99) await expect(button).toBeEnabled()
    if (index === 2 && previous) await expect.poll(async () => {
      return page.locator('.conversation-scroll').evaluate((node, old) => {
        const item = node.querySelector<HTMLElement>(`[data-message-key="${CSS.escape(old.key!)}"]`)
        return item ? Math.abs(item.getBoundingClientRect().top - node.getBoundingClientRect().top - old.offset) : 99999
      }, previous)
    }).toBeLessThan(5)
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
