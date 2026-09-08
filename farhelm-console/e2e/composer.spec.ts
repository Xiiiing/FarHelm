import AxeBuilder from '@axe-core/playwright'
import { expect, test, type Page } from '@playwright/test'
import type { SessionContext } from '../src/api/features'

export async function setup(page: Page, modern = true) {
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
    if (path.endsWith('/agents')) return route.fulfill({ json: { protocol: 'farhelm/1', agents: ['a', 'b'].map(id => ({ agent_id: id, hostname: id, agent_version: '0.8.0', last_seen_unix: 1, online: true, credential_state: 'paired', capabilities: modern ? ['codex.session_context', 'codex.model_choice', 'codex.native_identity'] : [] })) } })
    if (path.endsWith('/projects')) return route.fulfill({ json: { protocol: 'farhelm/1', projects: ['a', 'b'].map(id => ({ agent_id: id, candidate_id: 'same-candidate', suggested_project_id: 'p', display_name: '项目', state: 'approved' })) } })
    if (path.endsWith('/models')) return route.fulfill({ json: { models: [{ model: 'gpt-5.4', display_name: 'GPT-5.4', reasoning_efforts: ['medium', 'high'], default_reasoning_effort: 'medium', is_default: true }, { model: 'local-fast', display_name: 'Local Fast', reasoning_efforts: ['low'], default_reasoning_effort: 'low', is_default: false }] } })
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
  await model.click(); await expect(page.getByText(`当前模型 ${state.context.model} 不在可用目录中，仍可沿用原设置。`)).toBeVisible()
  await page.keyboard.press('Escape'); await page.getByRole('button', { name: '会话权限：可编辑' }).click()
  await expect(page.getByText('网页暂不支持人工审批；需要你批准的操作会被拒绝，不会自动放行。')).toBeVisible()
  await page.keyboard.press('Escape'); state.context = {}; await page.getByRole('button', { name: '刷新对话' }).click()
  await expect(page.getByRole('button', { name: '会话模型：选择模型' })).toBeVisible()
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

test('model choices use native options, remain per session and freeze retry settings', async ({ page }) => {
  await setup(page)
  const requests: { body: unknown; identity: string | undefined }[] = []
  await page.route('**/codex/sessions/s/messages', async route => {
    requests.push({ body: route.request().postDataJSON(), identity: route.request().headers()['idempotency-key'] })
    if (requests.length === 1) return route.fulfill({ status: 503, json: { error: 'agent_save_unconfirmed' } })
    return route.fulfill({ json: { state: 'accepted' } })
  })
  await page.goto('/codex?session=s')
  await page.getByRole('button', { name: /会话模型：gpt-5.4/ }).click()
  await page.getByRole('combobox', { name: '选择模型', exact: true }).click()
  await page.getByTitle('Local Fast', { exact: true }).click()
  await expect(page.getByRole('combobox', { name: '选择推理强度' })).toBeVisible()
  await page.keyboard.press('Escape')
  await expect(page.getByRole('button', { name: '会话模型：local-fast，推理强度低' })).toBeVisible()
  await page.getByLabel('给 Codex 发送指令').fill('使用选中的模型')
  await page.getByRole('button', { name: '发送指令' }).click()
  await expect(page.getByText('尚未确认 Agent 保存，重试会核对同一次操作')).toBeVisible()
  await page.getByRole('button', { name: /会话模型：local-fast/ }).click()
  await page.getByRole('combobox', { name: '选择模型', exact: true }).click()
  await page.getByTitle('GPT-5.4', { exact: true }).click()
  await page.keyboard.press('Escape')
  await page.getByRole('button', { name: '发送指令' }).click()
  await expect.poll(() => requests.length).toBe(2)
  expect(requests[0].body).toEqual({ prompt: '使用选中的模型', delivery: 'queue', model_choice: { model: 'local-fast', reasoning_effort: 'low' } })
  expect(requests[1]).toEqual(requests[0])
})

test('native identity exposes the same thread and renames only after server completion', async ({ page }) => {
  await setup(page)
  const names: string[] = []
  await page.route('**/codex/sessions/s/native', route => route.fulfill({ json: { session_id: 's', persisted: true, native_name: names.at(-1) ?? 'Codex session', source: 'vscode', history_mode: 'paginated' } }))
  await page.route('**/codex/sessions/s/name', async route => { names.push(route.request().postDataJSON().name); await new Promise(r => setTimeout(r, 250)); return route.fulfill({ json: { state: 'completed', data: { session_id: 's' } } }) })
  await page.goto('/codex?session=s')
  await page.getByRole('button', { name: '会话操作' }).click()
  await page.getByRole('menuitem', { name: '在原生 Codex 中继续' }).click()
  const dialog = page.getByRole('dialog', { name: '在原生 Codex 中继续' })
  await expect(dialog.getByText('会话已保存在服务器 Codex 中')).toBeVisible()
  await expect(dialog.locator('code')).toHaveText('codex resume s')
  await dialog.getByRole('button', { name: '重命名会话' }).click()
  const rename = page.getByRole('dialog', { name: '重命名会话' })
  await rename.getByLabel('会话名称').fill('跨端统一名称')
  await rename.getByRole('button', { name: '保存到 Codex' }).click()
  await expect(rename).toBeHidden()
  expect(names).toEqual(['跨端统一名称'])
})

test('composer has one surface and readable text without an inner focus frame', async ({ page }, info) => {
  await setup(page)
  for (const colorScheme of ['light', 'dark'] as const) {
    await page.emulateMedia({ colorScheme }); await page.goto('/codex?session=s')
    await page.getByLabel('给 Codex 发送指令').fill('中文输入与布局检查')
    await page.getByLabel('给 Codex 发送指令').focus()
    const values = await page.evaluate(() => {
      const style = (selector: string) => getComputedStyle(document.querySelector(selector)!)
      const input = style('.composer textarea')
      return { frame: style('.composer').backgroundColor, tools: [...document.querySelectorAll('.composer-control')].map(n => getComputedStyle(n).backgroundColor), border: input.borderTopWidth, outline: input.outlineStyle, body: style('.message-body').fontSize, list: style('.session-copy > span').fontSize, bottom: document.querySelector('.composer-wrap')!.getBoundingClientRect().bottom, height: innerHeight }
    })
    expect(values.tools.every(c => c === 'rgba(0, 0, 0, 0)')).toBe(true)
    expect(values.border).toBe('0px'); expect(values.outline).toBe('none')
    expect(parseFloat(values.body)).toBeGreaterThanOrEqual(info.project.name === 'mobile' ? 16 : 17)
    expect(parseFloat(values.list)).toBeGreaterThanOrEqual(15)
    expect(values.bottom).toBeLessThanOrEqual(values.height)
  }
})

test('native handoff reports busy safely and a deliberate retry uses a new operation', async ({ page }) => {
  await setup(page)
  let held = true
  const identities: string[] = []
  await page.route('**/codex/sessions/s/native', route => route.fulfill({ json: { session_id: 's', persisted: true, held_by_agent: held, native_name: '同一条原生会话' } }))
  await page.route('**/codex/sessions/s/handoff', route => {
    identities.push(route.request().headers()['idempotency-key'])
    return route.fulfill({ json: { command_id: `handoff-${identities.length}`, state: 'accepted' } })
  })
  await page.route('**/commands/handoff-*', route => {
    const failed = route.request().url().endsWith('handoff-1')
    if (!failed) held = false
    return route.fulfill({ json: { state: failed ? 'failed' : 'completed', detail: failed ? 'codex_handoff_busy' : undefined, data: { session_id: 's' } } })
  })
  await page.goto('/codex?session=s')
  await page.getByRole('button', { name: '会话操作' }).click()
  await page.getByRole('menuitem', { name: '在原生 Codex 中继续' }).click()
  const dialog = page.getByRole('dialog', { name: '在原生 Codex 中继续' })
  await dialog.getByRole('button', { name: '释放连接后继续' }).click()
  await expect(dialog.getByText('Agent 正在读取或执行对话，请结束活动任务后再交接')).toBeVisible()
  expect(held).toBe(true)
  await dialog.getByRole('button', { name: '释放连接后继续' }).click()
  await expect(dialog.getByText('已释放 FarHelm 的空闲连接，可以在原生客户端继续。')).toBeVisible()
  await expect(dialog.getByRole('button', { name: '释放连接后继续' })).toHaveCount(0)
  expect(new Set(identities).size).toBe(2)
})

test('model drafts belong to the selected session through rapid navigation', async ({ page }) => {
  await setup(page)
  const sessions = ['s', 'other'].map(id => ({ session_id: id, agent_id: 'a', project_id: 'p', title: id === 's' ? '会话设置验收' : '另一模型会话', mode: 'inspect', state: 'idle', updated_at_unix: 1 }))
  await page.route('**/codex/sessions?*', route => route.fulfill({ json: { protocol: 'farhelm/1', sessions } }))
  await page.route('**/session-display', route => route.fulfill({ json: { protocol: 'farhelm/1', sessions, incomplete_agents: [] } }))
  await page.route('**/codex/sessions/other', route => route.fulfill({ json: sessions[1] }))
  await page.route('**/codex/sessions/other/transcript*', route => route.fulfill({ json: { session_id: 'other', turns: [], context: { model: 'gpt-5.4', reasoning_effort: 'high' } } }))
  await page.goto('/codex?session=s')
  await page.getByRole('button', { name: /会话模型：/ }).click()
  await page.getByRole('combobox', { name: '选择模型', exact: true }).click()
  await page.getByTitle('Local Fast', { exact: true }).click()
  await page.keyboard.press('Escape')
  const choose = async (name: string) => {
    if (page.viewportSize()!.width < 768) await page.getByRole('button', { name: '打开会话列表' }).click()
    await page.getByRole('button', { name, exact: true }).filter({ visible: true }).click()
  }
  await choose('另一模型会话')
  await expect(page.getByRole('button', { name: /会话模型：gpt-5.4/ })).toBeVisible()
  await choose('会话设置验收')
  await expect(page.getByRole('button', { name: /会话模型：local-fast/ })).toBeVisible()
})
