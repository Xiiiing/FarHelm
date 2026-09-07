import { useQuery } from '@tanstack/react-query'
import { fetchAgents, type AgentListResponse } from '../api/agents'
export type AgentsState =
  | { state: 'loading'; data?: undefined; message?: undefined }
  | { state: 'ready'; data: AgentListResponse; message?: undefined }
  | { state: 'error'; data?: undefined; message: string }
export function useAgents() {
  const query = useQuery({ queryKey: ['agents'], queryFn: ({ signal }) => fetchAgents(signal), staleTime: 30_000, refetchInterval: 60_000 })
  const agents: AgentsState = query.data ? { state: 'ready', data: query.data } : query.error ? { state: 'error', message: query.error.message } : { state: 'loading' }
  return { agents, refresh: () => { void query.refetch() } }
}
