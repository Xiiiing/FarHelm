import { notification } from 'antd'
import { useEffect } from 'react'
import { useNavigate } from 'react-router-dom'
import { subscribeEvents } from '../api/events'
import { notices, stateLabel } from '../api/notifications'

export function LiveNotifications() {
  const [api, holder] = notification.useNotification(); const navigate = useNavigate()
  useEffect(() => {
    let active = true; let highWater: number | undefined; let refreshing = false; let dirty = false
    const baseline = async () => { try { const page = await notices(); if (active) highWater = page.latest_id } catch { /* The notifications page exposes connection errors. */ } }
    void baseline()
    const offOpen = subscribeEvents(['open'], () => { void baseline() })
    const refresh = () => {
      if (highWater === undefined) return
      if (refreshing) { dirty = true; return }
      refreshing = true
      void notices().then((page) => {
        if (!active) return
        const fresh = page.notifications.filter((item) => item.id > highWater! && (item.category === 'test' || (item.category === 'codex' ? page.preferences?.codex !== false : page.preferences?.experiments !== false)) && Date.now() / 1000 - item.created_at_unix < 30)
        highWater = Math.max(highWater!, page.latest_id)
        if (document.visibilityState === 'visible') for (const item of fresh) api.open({ key: String(item.id), title: `${item.title} · ${stateLabel(item.state)}`, description: item.agent_id, onClick: () => navigate(`/notifications?id=${item.id}`) })
      }).catch(() => {}).finally(() => { refreshing = false; if (dirty && active) { dirty = false; refresh() } })
    }
    const off = subscribeEvents(['notification.changed', 'experiment.updated', 'experiment.reported', 'codex.turn.completed', 'codex.turn.failed', 'codex.turn.orphaned'], refresh)
    return () => { active = false; off(); offOpen() }
  }, [api, navigate])
  return holder
}
