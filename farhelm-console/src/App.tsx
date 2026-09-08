import { clearMarkdownCache } from './components/codex/markdownService'
import { clearOperationReceipts } from './api/features'
import {
  BellOutlined,
  CodeOutlined,
  DashboardOutlined,
  DesktopOutlined,
  FileSearchOutlined,
  LogoutOutlined,
  MenuOutlined,
  MoonOutlined,
  MoreOutlined,
  SettingOutlined,
  SunOutlined,
  UnorderedListOutlined,
} from '@ant-design/icons'
import { QueryClientProvider } from '@tanstack/react-query'
import { queryClient, connectCodexCache } from './api/cache'
import { Avatar, Button, ConfigProvider, Drawer, Grid, Layout, Menu, Space, Spin, Tooltip, Typography } from 'antd'
import { lazy, Suspense, useEffect, useState } from 'react'
import { Navigate, Route, Routes, useLocation, useNavigate } from 'react-router-dom'

import { AuditPage } from './components/AuditPage'
import { SettingsPage } from './components/SettingsPage'
import { LiveNotifications } from './components/LiveNotifications'
import { ConnectionStatus } from './components/ConnectionStatus'
import type { ColorPreference } from './hooks/useColorMode'
import { AgentListPage } from './components/AgentListPage'
import { ExperimentPage } from './components/ExperimentPage'
import { LoginPage } from './components/LoginPage'
import { NotificationPage } from './components/NotificationPage'
import { Overview } from './components/Overview'
import { logout, readSession, type BrowserSession } from './api/auth'
import { useAgents } from './hooks/useAgents'
import { useColorMode } from './hooks/useColorMode'
import { useHubHealth } from './hooks/useHubHealth'
import { createTheme } from './theme'

const { Header, Content, Sider } = Layout
const CodexPage = lazy(() => import('./components/CodexPage').then((module) => ({ default: module.CodexPage })))
const codexFallback = <div className="session-loading"><Spin /><span>正在加载 Codex 工作区…</span></div>

const desktopItems = [
  { key: '/', icon: <DashboardOutlined aria-hidden />, label: '总览' },
  { key: '/agents', icon: <DesktopOutlined aria-hidden />, label: 'Agent' },
  { key: '/experiments', icon: <UnorderedListOutlined aria-hidden />, label: '实验' },
  { key: '/codex', icon: <CodeOutlined aria-hidden />, label: 'Codex' },
  { key: '/notifications', icon: <BellOutlined aria-hidden />, label: '通知' },
  { key: '/audit', icon: <FileSearchOutlined aria-hidden />, label: '审计' },
  { key: '/settings', icon: <SettingOutlined aria-hidden />, label: '设置' },
]

const mobileItems = [
  { key: '/', icon: <DashboardOutlined aria-hidden />, label: '总览' },
  { key: '/experiments', icon: <UnorderedListOutlined aria-hidden />, label: '实验' },
  { key: '/codex', icon: <CodeOutlined aria-hidden />, label: 'Codex' },
  { key: '/more', icon: <MoreOutlined aria-hidden />, label: '更多' },
]

function FeatureRoutes({ csrf, preference, onPreference, onLogout }: { csrf: string; preference: ColorPreference; onPreference: (value: ColorPreference) => void; onLogout: () => void }) {
  const { health, refresh } = useHubHealth()
  const { agents, refresh: refreshAgents } = useAgents()
  const refreshOverview = () => {
    refresh()
    refreshAgents()
  }
  return (
    <Routes>
      <Route path="/" element={<Overview health={health} agents={agents} onRefresh={refreshOverview} />} />
      <Route path="/agents" element={<AgentListPage csrf={csrf} agents={agents} onRefresh={refreshAgents} />} />
      <Route path="/experiments" element={<ExperimentPage />} />
      <Route path="/jobs" element={<Navigate to="/experiments" replace />} />
      <Route path="/codex" element={<Suspense fallback={codexFallback}><CodexPage csrf={csrf} agents={agents.data?.agents ?? []} /></Suspense>} />
      <Route path="/notifications" element={<NotificationPage csrf={csrf} />} />
      <Route path="/audit" element={<AuditPage />} />
      <Route path="/settings" element={<SettingsPage csrf={csrf} preference={preference} onPreference={onPreference} onLogout={onLogout} />} />
      <Route path="*" element={<Navigate to="/" replace />} />
    </Routes>
  )
}

function AppContent() {
  const screens = Grid.useBreakpoint()
  const isDesktop = Boolean(screens.md)
  const navigate = useNavigate()
  const location = useLocation()
  const [drawerOpen, setDrawerOpen] = useState(false)
  const { mode, preference, setPreference, toggleMode } = useColorMode()
  const [session, setSession] = useState<BrowserSession | null | undefined>(undefined)
  useEffect(() => { void readSession().then(setSession).catch(() => setSession(null)) }, [])
  useEffect(() => {
    if (!session) { queryClient.clear(); clearMarkdownCache(); clearOperationReceipts(); return }
    return connectCodexCache()
  }, [session])
  const mobileSelection = ['/agents', '/notifications', '/audit', '/settings'].includes(location.pathname)
    ? '/more'
    : location.pathname
  const currentPage = desktopItems.find((item) => item.key === location.pathname)?.label ?? '控制台'

  const go = (key: string) => {
    if (key === '/more') {
      setDrawerOpen(true)
      return
    }
    navigate(key)
    setDrawerOpen(false)
  }

  if (session === undefined) return <ConfigProvider theme={createTheme(mode)}><div className="session-loading"><Spin /><span>正在恢复安全会话…</span></div></ConfigProvider>
  if (session === null) return <ConfigProvider theme={createTheme(mode)}><LoginPage onLogin={setSession} /></ConfigProvider>

  return (
    <ConfigProvider theme={createTheme(mode)}>
      <LiveNotifications />
      <Layout className={location.pathname === '/codex' ? 'app-layout codex-shell' : 'app-layout'}>
        {isDesktop && (
          <Sider width={88} className="app-sider">
            <div className="brand" aria-label="FarHelm Console">
              <img src="/farhelm-mark.svg" alt="" width="36" height="36" />
              <strong>FarHelm</strong>
            </div>
            <nav aria-label="系统导航"><Menu mode="inline" selectedKeys={[location.pathname]} items={[
              { type: 'group', label: '工作空间', children: desktopItems.slice(0, 4) },
              { type: 'group', label: '管理', children: desktopItems.slice(4) },
            ]} onClick={({ key }) => go(key)} /></nav>
            <div className="sider-footer"><Tooltip title={`${session.user} · 账户设置`}><Button type="text" className="account-identity" aria-label={`账户：${session.user}，打开设置`} onClick={() => go('/settings')}><Avatar shape="square">{session.user.slice(0, 1).toUpperCase()}</Avatar></Button></Tooltip><span className="console-version">V0.8.0</span></div>
          </Sider>
        )}

        <Layout>
          <Header className="app-header">
            {isDesktop ? <div className="header-location"><span>{desktopItems.slice(4).some((item) => item.key === location.pathname) ? '管理' : '工作空间'}</span><span aria-hidden="true">/</span><strong>{currentPage}</strong></div> : <Typography.Text className="mobile-brand"><img src="/farhelm-mark.svg" width="24" height="24" alt="" />FarHelm</Typography.Text>}
            <Space className="header-actions">
              <ConnectionStatus />
              <Tooltip title={mode === 'dark' ? '浅色主题' : '深色主题'}>
              <Button
                className="theme-toggle"
                type="text"
                icon={<span key={mode} className="theme-glyph">{mode === 'dark' ? <SunOutlined /> : <MoonOutlined />}</span>}
                onClick={toggleMode}
                aria-label={mode === 'dark' ? '切换到浅色主题' : '切换到深色主题'}
              />
              </Tooltip>
              {isDesktop && <Tooltip title="退出登录"><Button type="text" icon={<LogoutOutlined />} aria-label="退出登录" onClick={() => void logout(session.csrf_token).then(() => setSession(null))} /></Tooltip>}
              {!isDesktop && <Button type="text" icon={<MenuOutlined />} onClick={() => setDrawerOpen(true)} aria-label="打开更多导航" />}
            </Space>
          </Header>
          <Content className={location.pathname === '/codex' ? 'app-content codex-content' : 'app-content'}><FeatureRoutes csrf={session.csrf_token} preference={preference} onPreference={setPreference} onLogout={() => void logout(session.csrf_token).then(() => setSession(null))} /></Content>
        </Layout>

        {!isDesktop && (
          <nav className="mobile-nav" aria-label="主要导航">
            {mobileItems.map((item) => (
              <button key={item.key} className={mobileSelection === item.key ? 'active' : ''} onClick={() => go(item.key)} aria-current={mobileSelection === item.key ? 'page' : undefined}>
                {item.icon}<span>{item.label}</span>
              </button>
            ))}
          </nav>
        )}

        <Drawer title="更多" placement="right" open={drawerOpen} onClose={() => setDrawerOpen(false)} size={Math.min(360, window.innerWidth)}>
          <Menu selectedKeys={[location.pathname]} items={desktopItems.slice(1)} onClick={({ key }) => go(key)} />
        </Drawer>
      </Layout>
    </ConfigProvider>
  )
}

export default function App() {
  return <QueryClientProvider client={queryClient}><AppContent /></QueryClientProvider>
}
