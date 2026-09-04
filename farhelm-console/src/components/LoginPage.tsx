import { LockOutlined, UserOutlined } from '@ant-design/icons'
import { Alert, Button, Card, Form, Input, Typography } from 'antd'
import { useState } from 'react'

import { login, type BrowserSession } from '../api/auth'

export function LoginPage({ onLogin }: { onLogin: (session: BrowserSession) => void }) {
  const [error, setError] = useState<string>()
  const [loading, setLoading] = useState(false)
  const submit = async (values: { username: string; password: string }) => {
    setLoading(true); setError(undefined)
    try { onLogin(await login(values.username, values.password)) }
    catch (reason) { setError(reason instanceof Error ? reason.message : '登录失败') }
    finally { setLoading(false) }
  }
  return <main className="login-page">
    <Card className="login-card">
      <div className="login-brand"><img src="/farhelm-mark.svg" alt="" width="48" height="48" /><div><Typography.Title level={1}>FarHelm</Typography.Title><Typography.Text type="secondary">实验与 Codex 安全控制台</Typography.Text></div></div>
      {error && <Alert showIcon type="error" title="无法登录" description={error} />}
      <Form layout="vertical" requiredMark={false} onFinish={submit}>
        <Form.Item label="用户名" name="username" rules={[{ required: true }]}><Input prefix={<UserOutlined />} autoComplete="username" /></Form.Item>
        <Form.Item label="密码" name="password" rules={[{ required: true }]}><Input.Password prefix={<LockOutlined />} autoComplete="current-password" /></Form.Item>
        <Button type="primary" htmlType="submit" loading={loading} block>登录</Button>
      </Form>
    </Card>
  </main>
}
