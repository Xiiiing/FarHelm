import { useEffect, useState } from 'react'

import { fetchHealth, type HealthResponse } from '../api/health'

type HubHealth =
  | { state: 'checking'; data?: undefined; message?: undefined }
  | { state: 'online'; data: HealthResponse; message?: undefined }
  | { state: 'offline'; data?: undefined; message: string }

export function useHubHealth() {
  const [health, setHealth] = useState<HubHealth>({ state: 'checking' })
  const [refreshToken, setRefreshToken] = useState(0)

  useEffect(() => {
    const controller = new AbortController()
    void fetchHealth(controller.signal)
      .then((data) => setHealth({ state: 'online', data }))
      .catch((error: unknown) => {
        if (error instanceof DOMException && error.name === 'AbortError') return
        const message = error instanceof Error ? error.message : 'Hub unavailable'
        setHealth({ state: 'offline', message })
      })
    return () => controller.abort()
  }, [refreshToken])

  return {
    health,
    refresh: () => {
      setHealth({ state: 'checking' })
      setRefreshToken((current) => current + 1)
    },
  }
}
