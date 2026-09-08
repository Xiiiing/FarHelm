import { CloudSyncOutlined, LoadingOutlined } from '@ant-design/icons'
import { Tooltip } from 'antd'
import { useSyncExternalStore } from 'react'
import { readEventConnection, subscribeEventConnection } from '../api/events'

export function ConnectionStatus() {
  const state = useSyncExternalStore(subscribeEventConnection, readEventConnection)
  const label = state === 'connected' ? '实时连接' : state === 'connecting' ? '正在连接' : '正在重连'
  return <Tooltip title={state === 'connected' ? '已连接 Hub，自动接收会话与任务更新' : '页面正在自动连接，已显示的内容和草稿会保留'}>
    <span className={`connection-status ${state}`} role="status" aria-label={label}>
      {state === 'connected' ? <span className="connection-dot" /> : state === 'connecting' ? <LoadingOutlined /> : <CloudSyncOutlined />}
      <span>{label}</span>
    </span>
  </Tooltip>
}
