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
import { Button, ConfigProvider, Drawer, Grid, Layout, Menu, Space, Typography } from 'antd'
import { useState } from 'react'
import { Navigate, Route, Routes, useLocation, useNavigate } from 'react-router-dom'

import { EmptyFeature } from './components/EmptyFeature'
import { Overview } from './components/Overview'
import { useColorMode } from './hooks/useColorMode'
import { useHubHealth } from './hooks/useHubHealth'
import { createTheme } from './theme'

const { Header, Content, Sider } = Layout

const desktopItems = [
  { key: '/', icon: <DashboardOutlined />, label: '总览' },
  { key: '/agents', icon: <DesktopOutlined />, label: 'Agent' },
  { key: '/jobs', icon: <UnorderedListOutlined />, label: '任务' },
  { key: '/codex', icon: <CodeOutlined />, label: 'Codex' },
  { key: '/notifications', icon: <BellOutlined />, label: '通知' },
  { key: '/audit', icon: <FileSearchOutlined />, label: '审计' },
  { key: '/settings', icon: <SettingOutlined />, label: '设置' },
]

const mobileItems = [
  { key: '/', icon: <DashboardOutlined />, label: '总览' },
  { key: '/jobs', icon: <UnorderedListOutlined />, label: '任务' },
  { key: '/codex', icon: <CodeOutlined />, label: 'Codex' },
  { key: '/more', icon: <MoreOutlined />, label: '更多' },
]

function FeatureRoutes() {
  const { health, refresh } = useHubHealth()
  return (
    <Routes>
      <Route path="/" element={<Overview health={health} onRefresh={refresh} />} />
      <Route path="/agents" element={<EmptyFeature title="Agent" description="训练服务器接入将在后续需求中实现。" icon={<DesktopOutlined className="empty-icon" />} />} />
      <Route path="/jobs" element={<EmptyFeature title="任务" description="任务编排与监控尚未接入真实数据。" icon={<UnorderedListOutlined className="empty-icon" />} />} />
      <Route path="/codex" element={<EmptyFeature title="Codex" description="真实 Codex 会话不在首轮骨架范围内。" icon={<CodeOutlined className="empty-icon" />} />} />
      <Route path="/notifications" element={<EmptyFeature title="通知" description="通知与 Web Push 将在可靠性设计完成后提供。" icon={<BellOutlined className="empty-icon" />} />} />
      <Route path="/audit" element={<EmptyFeature title="审计" description="审计记录尚未接入持久化存储。" icon={<FileSearchOutlined className="empty-icon" />} />} />
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
            <div className="sider-footer"><Typography.Text type="secondary">v0.1.0 · Skeleton</Typography.Text></div>
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
              {!isDesktop && <Button type="text" icon={<MenuOutlined />} onClick={() => setDrawerOpen(true)} aria-label="打开更多导航" />}
            </Space>
          </Header>
          <Content className="app-content"><FeatureRoutes /></Content>
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
