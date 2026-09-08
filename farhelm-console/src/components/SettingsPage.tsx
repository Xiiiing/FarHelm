import { Button, Card, Space, Typography } from 'antd'
import { AppearanceSettings, type AppearanceProps } from './AppearanceSettings'
import { NotificationSettings } from './NotificationSettings'
export function SettingsPage({ csrf, onLogout, ...appearance }: AppearanceProps & { csrf: string; onLogout: () => void }) {
  return <section className="feature-page settings-page"><header className="settings-heading"><Typography.Title level={1}>设置</Typography.Title><p>让 FarHelm 更合你的习惯。</p></header><div className="settings-sections">
    <AppearanceSettings {...appearance} />
    <NotificationSettings csrf={csrf} /><Card title="账户与版本"><Space><Typography.Text>FarHelm V0.8.0</Typography.Text><Button onClick={onLogout}>退出登录</Button></Space></Card>
  </div></section>
}
