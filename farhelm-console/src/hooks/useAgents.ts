import { useEffect, useState } from 'react'

import { fetchAgents, type AgentListResponse } from '../api/agents'

export type AgentsState =
  | { state: 'loading'; data?: undefined; message?: undefined }
  | { state: 'ready'; data: AgentListResponse; message?: undefined }
  | { state: 'error'; data?: undefined; message: string }

export function useAgents() {
  const [agents, setAgents] = useState<AgentsState>({ state: 'loading' })
  const [refreshToken, setRefreshToken] = useState(0)

  useEffect(() => {
    const controller = new AbortController()
    const load = () => {
      void fetchAgents(controller.signal)
        .then((data) => setAgents({ state: 'ready', data }))
        .catch((error: unknown) => {
          if (error instanceof DOMException && error.name === 'AbortError') return
          setAgents({
            state: 'error',
            message: error instanceof Error ? error.message : 'Agent 列表不可用',
          })
        })
    }
    load()
    const timer = window.setInterval(load, 15_000)
    return () => {
      window.clearInterval(timer)
      controller.abort()
    }
  }, [refreshToken])

  return {
    agents,
    refresh: () => {
      setAgents({ state: 'loading' })
      setRefreshToken((current) => current + 1)
    },
  }
}
