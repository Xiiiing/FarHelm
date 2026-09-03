export const PROTOCOL_VERSION = 'farhelm/1' as const

export type HealthResponse = {
  status: 'ok'
  service: 'farhelm-hub'
  version: string
  protocol: typeof PROTOCOL_VERSION
}

export async function fetchHealth(signal?: AbortSignal): Promise<HealthResponse> {
  const response = await fetch('/api/v1/health', {
    headers: { Accept: 'application/json' },
    signal,
  })

  if (!response.ok) {
    throw new Error(`Hub returned HTTP ${response.status}`)
  }

  const value = (await response.json()) as Partial<HealthResponse>
  if (
    value.status !== 'ok' ||
    value.service !== 'farhelm-hub' ||
    value.protocol !== PROTOCOL_VERSION ||
    typeof value.version !== 'string'
  ) {
    throw new Error('Hub returned an incompatible health response')
  }

  return value as HealthResponse
}
