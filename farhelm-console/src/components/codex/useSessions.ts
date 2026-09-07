import { useEffect, useMemo, useState } from 'react'
import { useInfiniteQuery, useQueries } from '@tanstack/react-query'
import { fetchSessionDisplay, fetchSessionPage, type DisplayPage } from '../../api/features'
import { keys, queryClient, mergeSession } from '../../api/cache'
import { errorText, formalTitle } from './presentation'
export type ArchiveFilter = 'false' | 'true' | 'all'

export function useSessions(csrf: string, query: string, archived: ArchiveFilter, agent?: string, project?: string) {
  const [search, setSearch] = useState(query)
  useEffect(() => { const timer = setTimeout(() => setSearch(query.trim()), 250); return () => clearTimeout(timer) }, [query])
  const filtered = Boolean(search || agent || project)
  const listing = useInfiniteQuery({
    queryKey: ['codex', 'sessions', archived, agent ?? '', project ?? '', search],
    initialPageParam: undefined as string | undefined,
    queryFn: async ({ pageParam, signal }): Promise<DisplayPage> => {
      const page = filtered
        ? await fetchSessionDisplay(csrf, { mode: 'search', query: search, archived, agent_id: agent, project_id: project, cursor: pageParam }, signal)
        : { ...await fetchSessionPage(undefined, archived, pageParam, signal), incomplete_agents: [] }
      page.sessions = page.sessions.map(mergeSession)
      for (const session of page.sessions) queryClient.setQueryData(keys.session(session.session_id), session)
      return page
    },
    getNextPageParam: (page) => page.next_cursor || undefined,
  })
  const pages = listing.data?.pages
  const labels = useQueries({ queries: (filtered ? [] : pages ?? []).map((page) => {
    const ids = page.sessions.filter((session) => !formalTitle(session.title)).map((session) => session.session_id)
    return { queryKey: ['codex', 'labels', ids], enabled: ids.length > 0, queryFn: ({ signal }: { signal: AbortSignal }) => fetchSessionDisplay(csrf, { mode: 'labels', session_ids: ids }, signal) }
  }) })
  const labelMap = new Map(labels.flatMap((result) => result.data?.sessions.map((row) => [row.session_id, row.display_label] as const) ?? []))
  const rows = useMemo(() => [...new Map(pages?.flatMap((page) => page.sessions).map((row) => [row.session_id, row]) ?? []).values()], [pages])
  const incomplete = [...new Map([...pages?.flatMap((page) => page.incomplete_agents) ?? [], ...labels.flatMap((result) => result.data?.incomplete_agents ?? [])].map((agent) => [agent.agent_id, agent])).values()]
  return {
    rows: rows.map((row) => labelMap.has(row.session_id) ? { ...row, display_label: labelMap.get(row.session_id) } : row),
    cursor: listing.hasNextPage ? pages?.at(-1)?.next_cursor : undefined,
    incomplete, loading: listing.isFetching, error: listing.error ? errorText(listing.error) : undefined,
    refresh: async () => { await listing.refetch(); await Promise.all(labels.map((result) => result.refetch())) },
    more: () => listing.fetchNextPage(),
  }
}
