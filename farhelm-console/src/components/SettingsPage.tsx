import { Button, Card, Segmented, Space, Typography } from 'antd'
import type { ColorPreference } from '../hooks/useColorMode'
import { NotificationSettings } from './NotificationSettings'
export function SettingsPage({ csrf, preference, onPreference, onLogout }: { csrf: string; preference: ColorPreference; onPreference: (value: ColorPreference) => void; onLogout: () => void }) {
  return <section className="feature-page"><Typography.Title level={1}>设置</Typography.Title><Space orientation="vertical" size={24} style={{ width: '100%' }}>
    <Card title="外观"><Segmented aria-label="主题" value={preference} onChange={onPreference} options={[{ value: 'system', label: '跟随系统' }, { value: 'light', label: '浅色' }, { value: 'dark', label: '深色' }]} /></Card>
    <NotificationSettings csrf={csrf} /><Card title="账户与版本"><Space><Typography.Text>FarHelm V0.8.0</Typography.Text><Button onClick={onLogout}>退出登录</Button></Space></Card>
  </Space></section>
}
