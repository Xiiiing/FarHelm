import { PROTOCOL_VERSION } from './health'

export type AgentSummary = {
  agent_id: string
  hostname: string
  agent_version: string
  last_seen_unix: number
  online: boolean
}

export type AgentListResponse = {
  protocol: typeof PROTOCOL_VERSION
  agents: AgentSummary[]
}

function isAgentSummary(value: unknown): value is AgentSummary {
  if (typeof value !== 'object' || value === null) return false
  const agent = value as Partial<AgentSummary>
  return (
    typeof agent.agent_id === 'string' &&
    typeof agent.hostname === 'string' &&
    typeof agent.agent_version === 'string' &&
    typeof agent.last_seen_unix === 'number' &&
    Number.isSafeInteger(agent.last_seen_unix) &&
    agent.last_seen_unix >= 0 &&
    typeof agent.online === 'boolean'
  )
}

export async function fetchAgents(signal?: AbortSignal): Promise<AgentListResponse> {
  const response = await fetch('/api/v1/agents', {
    headers: { Accept: 'application/json' },
    signal,
  })
  if (!response.ok) {
    throw new Error(response.status === 401 ? '管理员身份验证失败' : `Hub returned HTTP ${response.status}`)
  }
  const value = (await response.json()) as Partial<AgentListResponse>
  if (
    value.protocol !== PROTOCOL_VERSION ||
    !Array.isArray(value.agents) ||
    !value.agents.every(isAgentSummary)
  ) {
    throw new Error('Hub returned an incompatible agent list')
  }
  return value as AgentListResponse
}
