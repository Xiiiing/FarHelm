/// <reference lib="webworker" />

import { cleanupOutdatedCaches, precacheAndRoute } from 'workbox-precaching'

declare const self: ServiceWorkerGlobalScope & { __WB_MANIFEST: Array<{ url: string; revision?: string }> }

precacheAndRoute(self.__WB_MANIFEST)
cleanupOutdatedCaches()

self.addEventListener('push', (event) => {
  const payload = event.data?.json() as { summary?: string; event_id?: string; url?: string } | undefined
  if (!payload?.event_id) return
  event.waitUntil(self.registration.showNotification('FarHelm', {
    body: payload.summary ?? '实验或 Codex 状态已更新',
    icon: '/farhelm-mark.svg',
    tag: payload.event_id,
    data: { url: payload.url ?? '/' },
  }))
})

self.addEventListener('notificationclick', (event) => {
  event.notification.close()
  const url = String((event.notification.data as { url?: string } | undefined)?.url ?? '/')
  event.waitUntil(self.clients.openWindow(url))
})
