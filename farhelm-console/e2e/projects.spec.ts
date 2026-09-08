import { expect, test, type Page } from '@playwright/test'
import AxeBuilder from '@axe-core/playwright'

async function setup(page: Page) {
  const model = {
    revision: 0,
    preferences: [] as { agent_id: string; project_id: string; display_name: string | null; hidden: boolean; pinned: boolean }[],
    projects: [{ candidate_id: 'empty', agent_id: 'a', suggested_project_id: 'empty', display_name: '空项目', session_count: 0, state: 'approved', updated_at_unix: 1, last_activity_unix: 2 }, { candidate_id: 'p', agent_id: 'a', suggested_project_id: 'p', display_name: '共享项目', session_count: 70, state: 'approved', updated_at_unix: 1, last_activity_unix: 1 }, { candidate_id: 'b-p', agent_id: 'b', suggested_project_id: 'p', display_name: '共享项目', session_count: 0, state: 'approved', updated_at_unix: 1, last_activity_unix: 0 }, { candidate_id: 'pending', agent_id: 'a', suggested_project_id: 'pending', display_name: '待导入项目', session_count: 1, state: 'discovered', updated_at_unix: 1, last_activity_unix: 0 }],
    sessions: Array.from({ length: 70 }, (_, i) => ({ session_id: `s${i}`, agent_id: 'a', project_id: 'p', mode: 'inspect', state: 'idle', title: `会话 ${i}`, updated_at_unix: 100 - i, revision: 1 })),
    conflict: false, roots: false, created: undefined as { kind: string; directory_id: string; name?: string } | undefined,
  }
  await page.addInitScript(() => {
    const sources: EventTarget[] = []
    class Source extends EventTarget { constructor() { super(); sources.push(this) } close() {} }
    Object.assign(window, { EventSource: Source, projectEvent: () => sources.forEach((s) => s.dispatchEvent(new MessageEvent('project.preferences.updated', { data: JSON.stringify({ payload: {} }) }))) })
  })
  await page.route('**/api/v1/**', async (route) => {
    const url = new URL(route.request().url()); const path = url.pathname; const method = route.request().method()
    const data = method === 'POST' || method === 'PUT' ? route.request().postDataJSON() : undefined
    const send = (json: unknown, status = 200) => route.fulfill({ status, json })
    if (path.endsWith('/auth/session')) return send({ authenticated: true, user: 'admin', csrf_token: 'csrf', expires_at_unix: 2000000000 })
    if (path.endsWith('/agents')) return send({ protocol: 'farhelm/1', agents: ['a', 'b'].map((id) => ({ agent_id: id, hostname: id, agent_version: '0.11.0', online: true, last_seen_unix: 2000000000, capabilities: ['project.management', 'codex.session_context', 'codex.session_archive', 'codex.native_identity'] })) })
    if (path.endsWith('/project-preferences')) {
      if (method === 'PUT') {
        if (model.conflict || data.revision !== model.revision) { model.conflict = false; model.revision++; return send({ error: 'project_preferences_conflict' }, 409) }
        for (const p of data.projects) model.preferences = [...model.preferences.filter((v) => v.agent_id !== p.agent_id || v.project_id !== p.project_id), p]
        model.revision++
      }
      return send({ revision: model.revision, projects: model.preferences })
    }
    if (path.endsWith('/projects/import')) { for (const p of model.projects) if (data.candidate_ids.includes(p.candidate_id)) p.state = 'approved'; return send({ state: 'completed' }) }
    if (path.endsWith('/projects')) {
      if (method === 'POST') { model.created = data; model.projects.push({ ...model.projects[0], candidate_id: 'new', suggested_project_id: 'new', display_name: data.name ?? '已有目录' }); return send({ state: 'completed', data: { project_id: 'new' } }) }
      return send({ protocol: 'farhelm/1', projects: model.projects })
    }
    if (path.endsWith('/project-roots')) return send({ roots: model.roots ? [{ directory_id: 'rtd_00000000000000000000000000000000', name: '授权目录' }] : [] })
    if (path.endsWith('/project-directories')) { const id = url.searchParams.get('directory_id')!; return send({ directory: { directory_id: id, name: id.startsWith('rtd_') ? '授权目录' : '已有目录' }, parent_id: id.startsWith('rtd_') ? null : 'rtd_00000000000000000000000000000000', entries: id.startsWith('rtd_') ? [{ directory_id: 'dir_00000000000000000000000000000000', name: '已有目录' }] : [], next_cursor: null }) }
    if (path.endsWith('/info')) return send({ can_create_worktree: false })
    if (path.endsWith('/archive-preview')) return send({ session_ids: ['s0', 'child'], fingerprint: 'a'.repeat(64), can_archive: true, reason: null })
    if (path.endsWith('/archive') || path.endsWith('/unarchive')) { model.sessions[0].state = path.endsWith('/unarchive') ? 'idle' : 'archived'; model.sessions[0].revision++; return send({ state: 'completed' }) }
    if (path.endsWith('/codex/sessions')) {
      if (method === 'POST') { const id = `s${model.sessions.length}`; model.sessions.push({ session_id: id, agent_id: data.agent_id, project_id: data.project_id, mode: 'inspect', state: 'idle', title: '新会话', updated_at_unix: 101, revision: 1 }); return send({ state: 'completed', data: { session_id: id } }) }
      const visible = url.searchParams.get('visible_only') === 'true'; const offset = Number(url.searchParams.get('cursor') ?? 0)
      const sessions = model.sessions.filter((s) => (!visible || !model.preferences.some((p) => p.agent_id === s.agent_id && p.project_id === s.project_id && p.hidden)) && (!url.searchParams.get('project') || url.searchParams.get('project') === s.project_id))
      return send({ protocol: 'farhelm/1', sessions: sessions.slice(offset, offset + 50), next_cursor: sessions.length > offset + 50 ? String(offset + 50) : null })
    }
    if (path.endsWith('/session-display')) return send({ sessions: model.sessions.filter((s) => !data.visible_only || !model.preferences.some((p) => p.agent_id === s.agent_id && p.project_id === s.project_id && p.hidden)), incomplete_agents: [] })
    if (path.endsWith('/transcript')) return send({ session_id: 's0', turns: [] })
    if (/\/codex\/sessions\/s\d+$/.test(path)) return send(model.sessions.find((s) => path.endsWith(`/${s.session_id}`)))
    return send({ protocol: 'farhelm/1', experiments: [], schedules: [], notifications: [], latest_id: 0, unread_count: 0 })
  })
  return model
}
async function manage(page: Page) {
  await expect(page.getByRole('button', { name: '会话操作' })).toBeVisible()
  if (await page.getByRole('button', { name: '打开会话列表' }).isVisible()) await page.getByRole('button', { name: '打开会话列表' }).click()
  await page.getByRole('button', { name: '项目管理', exact: true }).filter({ visible: true }).click()
  return page.getByRole('dialog', { name: '项目管理', exact: true })
}

test('complete catalog, empty selection, account refresh and conflicts preserve current draft', async ({ page }) => {
  const model = await setup(page); await page.goto('/codex?session=s0')
  await page.getByLabel('给 Codex 发送指令').fill('保留当前草稿')
  let drawer = await manage(page)
  await expect(drawer.getByText('空项目', { exact: true })).toBeVisible()
  await expect(drawer.getByText('共享项目', { exact: true })).toHaveCount(2)
  model.conflict = true
  await drawer.getByRole('button', { name: '取消全选', exact: true }).click()
  await expect(drawer.getByText(/展示设置已在另一台设备更新/)).toBeVisible()
  await drawer.getByRole('button', { name: '取消全选', exact: true }).click()
  await expect.poll(() => model.preferences.filter((p) => p.hidden).length).toBe(3)
  await page.keyboard.press('Escape')
  await expect(page).toHaveURL(/session=s0/)
  await expect(page.getByLabel('给 Codex 发送指令')).toHaveValue('保留当前草稿')
  await page.getByRole('button', { name: '恢复项目显示' }).filter({ visible: true }).last().click()
  await expect.poll(() => model.preferences.find((p) => p.agent_id === 'a' && p.project_id === 'p')?.hidden).toBe(false)
  model.preferences[0].display_name = '跨设备名称'; model.revision++
  await page.evaluate(() => (window as unknown as { projectEvent: () => void }).projectEvent())
  drawer = await manage(page); await expect(drawer.getByText('跨设备名称', { exact: true })).toBeVisible()
  await drawer.getByRole('button', { name: '全选展示', exact: true }).click()
  await page.keyboard.press('Escape'); await expect(page.getByLabel('给 Codex 发送指令')).toHaveValue('保留当前草稿')
})

test('three add modes and project-specific creation work without Codex history', async ({ page }) => {
  const model = await setup(page); await page.goto('/codex?session=s0'); const drawer = await manage(page)
  await drawer.getByRole('tab', { name: '添加项目', exact: true }).click()
  await drawer.getByRole('checkbox', { name: /选择导入 待导入项目/ }).check(); await drawer.getByRole('button', { name: '导入所选（1）' }).click()
  await expect.poll(() => model.projects[3].state).toBe('approved')
  await drawer.getByText('接入已有目录', { exact: true }).click()
  await expect(drawer.getByText('尚未授权项目根目录')).toBeVisible()
  model.roots = true; await drawer.getByRole('button', { name: '刷新授权目录' }).click()
  await drawer.getByLabel('授权根目录').click(); await page.getByTitle('授权目录', { exact: true }).click()
  await expect(drawer.getByRole('button', { name: '接入当前目录' })).toBeDisabled()
  await drawer.getByRole('button', { name: '已有目录', exact: true }).click()
  await drawer.getByRole('button', { name: '接入当前目录' }).click()
  await expect.poll(() => model.created?.kind).toBe('attach')
  await drawer.getByRole('tab', { name: '添加项目', exact: true }).click(); await drawer.getByText('创建空项目', { exact: true }).click()
  await drawer.getByLabel('新项目名称').fill('新空项目')
  await drawer.getByRole('button', { name: '创建并接入空项目' }).click()
  await expect.poll(() => model.created?.name).toBe('新空项目')
  const empty = drawer.locator('.project-manager-row').filter({ hasText: '空项目' }).first()
  await empty.getByRole('button', { name: '新建会话', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: '创建 Codex 会话' }); await expect(dialog).toBeVisible()
  await expect(dialog).toContainText('空项目 · a'); await expect(dialog.getByRole('checkbox', { name: '使用隔离工作区' })).toBeDisabled()
})

test('archive confirmation shows descendants and restore retains session and draft', async ({ page }) => {
  await setup(page); await page.goto('/codex?session=s0'); await page.getByLabel('给 Codex 发送指令').fill('恢复后继续')
  await page.getByRole('button', { name: '会话操作' }).click(); await page.getByRole('menuitem', { name: '归档会话' }).click()
  const dialog = page.getByRole('dialog', { name: '归档会话', exact: true }); await expect(dialog).toContainText('影响范围：2 条会话'); await expect(dialog).toContainText('child')
  await dialog.getByRole('button', { name: '确认归档' }).click(); await expect(page.getByRole('button', { name: '恢复会话', exact: true })).toBeVisible()
  await expect(page).toHaveURL(/session=s0/); await expect(page.getByLabel('给 Codex 发送指令')).toHaveValue('恢复后继续')
  await page.getByRole('button', { name: '会话操作' }).click()
  await expect(page.getByRole('menuitem', { name: '重命名会话' })).toBeDisabled()
  await expect(page.getByRole('menuitem', { name: '在原生 Codex 中继续' })).toBeDisabled()
  await page.keyboard.press('Escape')
  await page.getByRole('button', { name: '恢复会话', exact: true }).click(); await page.getByRole('button', { name: '确认恢复' }).click()
  await expect(page.getByLabel('给 Codex 发送指令')).toBeEnabled(); await expect(page.getByLabel('给 Codex 发送指令')).toHaveValue('恢复后继续')
})

test('server project manager creates a preselected session and opens its conversation', async ({ page }) => {
  await setup(page); await page.goto('/agents'); await page.getByRole('button', { name: '添加与管理项目' }).click()
  const drawer = page.getByRole('dialog', { name: '项目管理', exact: true }); await drawer.getByRole('tab', { name: '展示管理' }).click()
  await drawer.locator('.project-manager-row').filter({ hasText: '空项目' }).getByRole('button', { name: '新建会话', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: '创建 Codex 会话' }); await expect(dialog).toContainText('空项目 · a')
  await dialog.getByRole('button', { name: '创建会话', exact: true }).click()
  await expect(page).toHaveURL(/\/codex\?session=s70$/); await expect(page.getByLabel('给 Codex 发送指令')).toBeEnabled()
})

for (const theme of ['light', 'dark'] as const) test(`project drawer keyboard and ${theme} theme`, async ({ page }) => {
  await page.emulateMedia({ colorScheme: theme, reducedMotion: 'reduce' }); await setup(page); await page.goto('/codex?session=s0'); const drawer = await manage(page)
  await expect(drawer.getByRole('button', { name: '取消全选' })).toBeVisible()
  const results = await new AxeBuilder({ page }).include('[role=dialog][aria-label=项目管理]').withTags(['wcag2a', 'wcag2aa']).analyze(); expect(results.violations).toEqual([])
  await page.keyboard.press('Escape'); await expect(drawer).toBeHidden()
  await expect(page.getByRole('button', { name: page.viewportSize()!.width < 768 ? '打开会话列表' : '项目管理', exact: true }).filter({ visible: true })).toBeFocused()
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false)
})

test('project headings retain readable width alongside the menu', async ({ page }) => {
  await setup(page); await page.goto('/codex?session=s0'); await expect(page.getByRole('button', { name: '会话操作' })).toBeVisible()
  if (page.viewportSize()!.width < 768) await page.getByRole('button', { name: '打开会话列表' }).click()
  const header = page.locator('.codex-rail:visible .project-group-header').first()
  const heading = header.locator('.session-group-heading')
  const box = await header.boundingBox(); const bounds = await heading.boundingBox()
  expect(bounds!.width).toBeGreaterThan(box!.width - 60)
  expect((await heading.locator('.project-name').boundingBox())!.width).toBeGreaterThan(80)
})
