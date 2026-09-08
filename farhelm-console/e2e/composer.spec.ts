import AxeBuilder from '@axe-core/playwright'
import { expect, test, type Page } from '@playwright/test'
import type { SessionContext } from '../src/api/features'

async function setup(page: Page, modern = true) {
  const context: SessionContext = { model: 'gpt-5.4', reasoning_effort: 'high', sandbox: 'danger-full-access', approval_policy: 'never' }
  const state = { context: modern ? context : undefined, reads: 0, creations: [] as { agent_id: string; mode: string; inherit_permissions?: boolean }[] }
  const session = { session_id: 's', agent_id: 'a', project_id: 'p', title: '会话设置验收', mode: 'inspect', state: 'idle', updated_at_unix: 1 }
  await page.addInitScript(() => {
    const sources: EventTarget[] = []
    class Source extends EventTarget { constructor() { super(); sources.push(this) } close() {} }
    Object.assign(window, { EventSource: Source, emitSettingsTest: (type: string, payload: unknown) => sources.forEach(s => s.dispatchEvent(new MessageEvent(type, { data: JSON.stringify({ payload }) }))) })
  })
  await page.route('**/api/v1/**', async route => {
    const path = new URL(route.request().url()).pathname
    if (path.endsWith('/auth/session')) return route.fulfill({ json: { authenticated: true, user: 'admin', csrf_token: 'test', expires_at_unix: 2000000000 } })
    if (path.endsWith('/agents')) return route.fulfill({ json: { protocol: 'farhelm/1', agents: ['a', 'b'].map(id => ({ agent_id: id, hostname: id, agent_version: '0.8.0', last_seen_unix: 1, online: true, credential_state: 'paired', capabilities: modern ? ['codex.session_context'] : [] })) } })
    if (path.endsWith('/projects')) return route.fulfill({ json: { protocol: 'farhelm/1', projects: ['a', 'b'].map(id => ({ agent_id: id, candidate_id: 'same-candidate', suggested_project_id: 'p', display_name: '项目', state: 'approved' })) } })
    if (path.endsWith('/transcript')) { state.reads++; return route.fulfill({ json: { session_id: 's', turns: [{ turn_id: 't', status: 'completed', items: [{ item_id: 'i', kind: 'assistant_message', text: '这是合成验收内容。' }] }], context: state.context } }) }
    if (path.endsWith('/codex/sessions') && route.request().method() === 'POST') { state.creations.push(route.request().postDataJSON()); return route.fulfill({ json: { state: 'completed', data: { session_id: 's' } } }) }
    if (path.endsWith('/codex/sessions') || path.endsWith('/session-display')) return route.fulfill({ json: { protocol: 'farhelm/1', sessions: [session], incomplete_agents: [] } })
    if (path.endsWith('/codex/sessions/s')) return route.fulfill({ json: session })
    return route.fulfill({ json: { protocol: 'farhelm/1', notifications: [], unread_count: 0, latest_id: 0 } })
  })
  return state
}

test('composer controls share a 44px axis and archive tabs fill the rail in light and dark', async ({ page }, info) => {
  await setup(page)
  const sizes = info.project.name === 'mobile' ? [[390, 844]] : [[1440, 900], [2550, 1233]]
  for (const [width, height] of sizes) for (const colorScheme of ['light', 'dark'] as const) {
    await page.setViewportSize({ width, height }); await page.emulateMedia({ colorScheme }); await page.goto('/codex?session=s')
    await expect(page.getByRole('button', { name: '会话模型：gpt-5.4，推理强度高' })).toBeVisible()
    await expect(page.getByRole('button', { name: '会话权限：完全访问' })).toBeVisible()
    await page.evaluate(() => document.fonts.ready)
    const bounds = await page.locator('.composer-actions .ant-btn').evaluateAll(nodes => nodes.map(n => { const r = n.getBoundingClientRect(); return { height: r.height, y: r.y, right: r.right } }))
    expect(bounds).toHaveLength(4)
    for (const box of bounds) { expect(box.height).toBe(44); expect(Math.abs(box.y - bounds[0].y)).toBeLessThan(1); expect(box.right).toBeLessThan(width) }
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    const violations = (await new AxeBuilder({ page }).analyze()).violations.filter(v => ['serious', 'critical'].includes(v.impact!))
    expect(violations).toEqual([])
    if (width < 768) await page.getByRole('button', { name: '打开会话列表' }).click()
    const rail = page.locator('.codex-rail:visible')
    const shape = await rail.evaluate(n => { const tabs = n.querySelector('.archive-tabs')!; return { inner: n.clientWidth - 24, tabs: tabs.getBoundingClientRect().width, items: [...tabs.querySelectorAll('.ant-segmented-item')].map(n => n.getBoundingClientRect().width) } })
    expect(Math.abs(shape.tabs - shape.inner)).toBeLessThan(1)
    expect(Math.max(...shape.items) - Math.min(...shape.items)).toBeLessThan(1)
    await expect(rail.locator('.codex-rail-head').getByRole('button', { name: '筛选服务器与项目' })).toBeVisible()
  }
})

test('native model and permissions remain session facts, with readable long and missing values', async ({ page }) => {
  const state = await setup(page); await page.goto('/codex?session=s')
  await page.getByRole('button', { name: '会话权限：完全访问' }).click()
  await expect(page.getByText('无需人工批准', { exact: true })).toBeVisible()
  await expect(page.getByText('权限由服务器上的 Codex 管理，此处展示实际设置。')).toBeVisible()
  await page.keyboard.press('Escape')
  state.context = { model: 'custom-model-'.repeat(9), reasoning_effort: 'medium', sandbox: 'workspace-write', approval_policy: 'on-request', approvals_reviewer: 'user' }
  await page.getByRole('button', { name: '刷新对话' }).click()
  const model = page.getByRole('button', { name: /会话模型：custom-model-/ }); await expect(model).toBeVisible()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await model.click(); await expect(page.locator('.session-settings-details:visible dd').first()).toHaveText(state.context.model!)
  await page.keyboard.press('Escape'); await page.getByRole('button', { name: '会话权限：可编辑' }).click()
  await expect(page.getByText('网页暂不支持人工审批；需要你批准的操作会被拒绝，不会自动放行。')).toBeVisible()
  await page.keyboard.press('Escape'); state.context = {}; await page.getByRole('button', { name: '刷新对话' }).click()
  await expect(page.getByRole('button', { name: '会话模型：模型未提供' })).toBeVisible()
  await expect(page.getByRole('button', { name: '会话权限：跟随 Codex' })).toBeVisible()
  await page.getByLabel('给 Codex 发送指令').fill('保留草稿')
  state.context = { model: 'gpt-5.4', sandbox: 'read-only', approval_policy: 'on-request' }
  await page.evaluate(() => (window as unknown as { emitSettingsTest: (type: string, data: object) => void }).emitSettingsTest('codex.turn.started', { session_id: 's', data: { turn_id: 't' } }))
  await expect(page.getByRole('button', { name: '会话权限：仅分析' })).toBeVisible()
  await expect(page.getByLabel('给 Codex 发送指令')).toHaveValue('保留草稿')
})

test('new sessions inherit native settings and isolate only when explicitly selected', async ({ page }) => {
  const state = await setup(page); await page.goto('/codex?session=s')
  for (const isolated of [false, true]) {
    if (page.viewportSize()!.width < 768) await page.getByRole('button', { name: '打开会话列表' }).click()
    await page.getByRole('button', { name: '新建会话', exact: true }).click()
    const dialog = page.getByRole('dialog', { name: '创建 Codex 会话' })
    await dialog.getByRole('combobox').click(); await page.getByTitle('项目 · b', { exact: true }).click()
    if (isolated) await dialog.getByRole('checkbox', { name: '使用隔离工作区' }).check()
    await dialog.getByRole('button', { name: '创建会话', exact: true }).click()
    await expect(dialog).toBeHidden()
    expect(state.creations.at(-1)).toEqual({ agent_id: 'b', project_id: 'p', mode: isolated ? 'edit' : 'inspect', inherit_permissions: true })
  }
})

test('older Agents are not represented as inheriting native permissions', async ({ page }) => {
  const state = await setup(page, false); await page.goto('/codex?session=s')
  await page.getByRole('button', { name: '会话权限：仅分析' }).click()
  await expect(page.getByText('当前 Agent 使用旧的权限逻辑，请升级 Agent 后使用会话权限继承。')).toBeVisible()
  await page.keyboard.press('Escape')
  if (page.viewportSize()!.width < 768) await page.getByRole('button', { name: '打开会话列表' }).click()
  await page.getByRole('button', { name: '新建会话', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: '创建 Codex 会话' })
  await dialog.getByRole('combobox').click(); await page.getByTitle('项目 · a', { exact: true }).click()
  await expect(dialog.getByRole('button', { name: '创建会话', exact: true })).toBeDisabled()
  expect(state.creations).toEqual([])
})
