import { BellOutlined, CheckCircleOutlined } from '@ant-design/icons'
import { Alert, Button, Card, Space, Tag, Typography } from 'antd'
import { useEffect, useState } from 'react'

import { currentPushSubscription, disablePush, enablePush, pushSupported } from '../api/features'

export function NotificationPage({ csrf }: { csrf: string }) {
  const [enabled, setEnabled] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string>()
  const supported = pushSupported()

  useEffect(() => {
    if (supported) void currentPushSubscription().then((subscription) => setEnabled(Boolean(subscription)))
  }, [supported])

  const toggle = async () => {
    setBusy(true)
    setError(undefined)
    try {
      if (enabled) await disablePush(csrf)
      else await enablePush(csrf)
      setEnabled(!enabled)
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : '通知设置失败')
    } finally {
      setBusy(false)
    }
  }

  return (
    <section aria-labelledby="notifications-title" className="feature-page">
      <div className="page-heading">
        <div>
          <Typography.Text className="eyebrow">WEB PUSH</Typography.Text>
          <Typography.Title id="notifications-title" level={1}>通知</Typography.Title>
          <Typography.Paragraph type="secondary">PWA 关闭后仍可收到实验和 Codex 结果提醒。</Typography.Paragraph>
        </div>
      </div>
      {error && <Alert showIcon type="error" title="通知设置失败" description={error} />}
      <Card title="后台通知" className="agent-list-card">
        <Space direction="vertical" size={16}>
          {enabled
            ? <Tag color="success" icon={<CheckCircleOutlined />}>此设备已订阅</Tag>
            : <Tag icon={<BellOutlined />}>{supported ? '此设备尚未订阅' : '当前浏览器不支持 Web Push'}</Tag>}
          <Typography.Paragraph type="secondary">
            通知只包含摘要、事件 ID 和会话深链，不包含日志、源码或 prompt。
          </Typography.Paragraph>
          <Button type={enabled ? 'default' : 'primary'} disabled={!supported} loading={busy} onClick={() => void toggle()}>
            {enabled ? '关闭此设备通知' : '启用此设备通知'}
          </Button>
        </Space>
      </Card>
    </section>
  )
}
