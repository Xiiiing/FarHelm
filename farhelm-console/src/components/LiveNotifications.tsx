import { notification } from 'antd'
import { useEffect } from 'react'
import { useQuery } from '@tanstack/react-query'
import { useNavigate } from 'react-router-dom'
import { subscribeEvents } from '../api/events'
import { queryClient } from '../api/cache'
import { notices, stateLabel, type Notice, type NoticePage } from '../api/notifications'

export function LiveNotifications() {
  const [api, holder] = notification.useNotification(); const navigate = useNavigate()
  useQuery({ queryKey: ['notifications', 'latest'], queryFn: () => notices() })
  useEffect(() => {
    const seen = new Set<number>()
    let opened = false
    const show = (item: Notice) => {
      const page = queryClient.getQueryData<NoticePage>(['notifications', 'latest'])
      if (seen.has(item.id)) return
      seen.add(item.id); if (seen.size > 256) seen.delete(seen.values().next().value!)
      if (!page || item.id <= page.latest_id) return
      queryClient.setQueryData<NoticePage>(['notifications', 'latest'], { ...page, notifications: [item, ...page.notifications].slice(0, 30), unread_count: page.unread_count + (item.read_at_unix ? 0 : 1), latest_id: item.id })
      const enabled = item.category === 'test' || (item.category === 'codex' ? page.preferences?.codex !== false : page.preferences?.experiments !== false)
      if (enabled && Date.now() / 1000 - item.created_at_unix < 30 && document.visibilityState === 'visible') api.open({ key: String(item.id), title: `${item.title} · ${stateLabel(item.state)}`, description: item.agent_id, onClick: () => navigate(`/notifications?id=${item.id}`) })
    }
    const off = subscribeEvents(['open', 'notification.created', 'notification.changed'], (event) => {
      if (event.type === 'open') { if (opened) void queryClient.invalidateQueries({ queryKey: ['notifications'] }); opened = true; return }
      if (event.type === 'notification.changed') { void queryClient.invalidateQueries({ queryKey: ['notifications'] }); return }
      try { const { payload } = JSON.parse(event.data) as { payload: Notice }; if (Number.isSafeInteger(payload.id)) show(payload) } catch { /* Malformed notices are ignored. */ }
    })
    return off
  }, [api, navigate])
  return holder
}
