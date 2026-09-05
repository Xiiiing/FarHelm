import {
  BellOutlined,
  CodeOutlined,
  DashboardOutlined,
  DesktopOutlined,
  FileSearchOutlined,
  MenuOutlined,
  MoonOutlined,
  MoreOutlined,
  SettingOutlined,
  SunOutlined,
  UnorderedListOutlined,
} from '@ant-design/icons'
import { Button, ConfigProvider, Drawer, Grid, Layout, Menu, Space, Spin, Typography } from 'antd'
import { lazy, Suspense, useEffect, useState } from 'react'
import { Navigate, Route, Routes, useLocation, useNavigate } from 'react-router-dom'

import { EmptyFeature } from './components/EmptyFeature'
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
  { key: '/', icon: <DashboardOutlined />, label: '总览' },
  { key: '/agents', icon: <DesktopOutlined />, label: 'Agent' },
  { key: '/experiments', icon: <UnorderedListOutlined />, label: '实验' },
  { key: '/codex', icon: <CodeOutlined />, label: 'Codex' },
  { key: '/notifications', icon: <BellOutlined />, label: '通知' },
  { key: '/audit', icon: <FileSearchOutlined />, label: '审计' },
  { key: '/settings', icon: <SettingOutlined />, label: '设置' },
]

const mobileItems = [
  { key: '/', icon: <DashboardOutlined />, label: '总览' },
  { key: '/experiments', icon: <UnorderedListOutlined />, label: '实验' },
  { key: '/codex', icon: <CodeOutlined />, label: 'Codex' },
  { key: '/more', icon: <MoreOutlined />, label: '更多' },
]

function FeatureRoutes({ csrf }: { csrf: string }) {
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
      <Route path="/codex" element={<Suspense fallback={codexFallback}><CodexPage csrf={csrf} /></Suspense>} />
      <Route path="/notifications" element={<NotificationPage csrf={csrf} />} />
      <Route path="/audit" element={<EmptyFeature title="审计" description="命令与结果已持久化；独立审计查询界面暂未开放。" icon={<FileSearchOutlined className="empty-icon" />} />} />
      <Route path="/settings" element={<EmptyFeature title="设置" description="配置项会在安全模型确定后开放。" icon={<SettingOutlined className="empty-icon" />} />} />
      <Route path="*" element={<Navigate to="/" replace />} />
    </Routes>
  )
}

export default function App() {
  const screens = Grid.useBreakpoint()
  const isDesktop = Boolean(screens.md)
  const navigate = useNavigate()
  const location = useLocation()
  const [drawerOpen, setDrawerOpen] = useState(false)
  const { mode, toggleMode } = useColorMode()
  const [session, setSession] = useState<BrowserSession | null | undefined>(undefined)
  useEffect(() => { void readSession().then(setSession).catch(() => setSession(null)) }, [])
  const mobileSelection = ['/agents', '/notifications', '/audit', '/settings'].includes(location.pathname)
    ? '/more'
    : location.pathname

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
  if (location.pathname === '/codex') return <ConfigProvider theme={createTheme(mode)}><Suspense fallback={codexFallback}><CodexPage csrf={session.csrf_token} /></Suspense></ConfigProvider>

  return (
    <ConfigProvider theme={createTheme(mode)}>
      <Layout className="app-layout">
        {isDesktop && (
          <Sider width={240} className="app-sider">
            <div className="brand" aria-label="FarHelm Console">
              <img src="/farhelm-mark.svg" alt="" width="36" height="36" />
              <div><strong>FarHelm</strong><span>远程训练控制台</span></div>
            </div>
            <Menu mode="inline" selectedKeys={[location.pathname]} items={desktopItems} onClick={({ key }) => go(key)} />
            <div className="sider-footer"><Typography.Text type="secondary">V0.6.0 · Codex workspace</Typography.Text></div>
          </Sider>
        )}

        <Layout>
          <Header className="app-header">
            {!isDesktop && <Typography.Text className="mobile-brand">FarHelm</Typography.Text>}
            <Space className="header-actions">
              <Button
                type="text"
                icon={mode === 'dark' ? <SunOutlined /> : <MoonOutlined />}
                onClick={toggleMode}
                aria-label={mode === 'dark' ? '切换到浅色主题' : '切换到深色主题'}
              />
              {isDesktop && <Button onClick={() => void logout(session.csrf_token).then(() => setSession(null))}>退出</Button>}
              {!isDesktop && <Button type="text" icon={<MenuOutlined />} onClick={() => setDrawerOpen(true)} aria-label="打开更多导航" />}
            </Space>
          </Header>
          <Content className="app-content"><FeatureRoutes csrf={session.csrf_token} /></Content>
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
