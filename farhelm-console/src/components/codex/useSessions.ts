import { useCallback, useEffect, useRef, useState } from 'react'
import { fetchSessionDisplay, fetchSessionPage, json, type CodexSession, type DisplayPage } from '../../api/features'
import { subscribeEvents } from '../../api/events'
import { errorText } from './presentation'

export type ArchiveFilter = 'false' | 'true' | 'all'
export function useSessions(csrf: string, query: string, archived: ArchiveFilter, agent?: string, project?: string) {
  const [rows, setRows] = useState<CodexSession[]>([])
  const [cursor, setCursor] = useState<string>()
  const [incomplete, setIncomplete] = useState<DisplayPage['incomplete_agents']>([])
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string>()
  const generation = useRef(0)
  const loadingRef = useRef(false)
  const controller = useRef<AbortController>(undefined)
  const filtered = Boolean(query.trim() || agent || project)
  const load = useCallback(async (next?: string) => {
    if (next && loadingRef.current) return
    const epoch = ++generation.current
    controller.current?.abort(); const abort = new AbortController(); controller.current = abort
    loadingRef.current = true; setLoading(true); setError(undefined)
    try {
      if (filtered) {
        const page = await fetchSessionDisplay(csrf, { mode: 'search', query: query.trim(), archived, agent_id: agent, project_id: project, cursor: next }, abort.signal)
        if (epoch !== generation.current) return
        setRows((old) => next ? [...old, ...page.sessions.filter((s) => !old.some((r) => r.session_id === s.session_id))] : page.sessions)
        setCursor(page.next_cursor); setIncomplete(page.incomplete_agents)
      } else {
        const page = await fetchSessionPage(undefined, archived, next)
        if (epoch !== generation.current) return
        setRows((old) => next ? [...old, ...page.sessions.filter((s) => !old.some((r) => r.session_id === s.session_id))] : page.sessions)
        setCursor(page.next_cursor)
        if (page.sessions.length) {
          const display = await fetchSessionDisplay(csrf, { mode: 'labels', session_ids: page.sessions.map((s) => s.session_id) }, abort.signal)
          if (epoch !== generation.current) return
          const labels = new Map(display.sessions.map((s) => [s.session_id, s.display_label]))
          setRows((old) => old.map((s) => labels.has(s.session_id) ? { ...s, display_label: labels.get(s.session_id) } : s)); setIncomplete(display.incomplete_agents)
        } else setIncomplete([])
      }
    } catch (reason) { if (epoch === generation.current && !abort.signal.aborted) setError(errorText(reason)) }
    finally { if (epoch === generation.current) { loadingRef.current = false; setLoading(false) } }
  }, [agent, archived, csrf, filtered, project, query])
  useEffect(() => {
    const requestGeneration = generation; const requestController = controller
    ++requestGeneration.current; requestController.current?.abort(); loadingRef.current = false
    const timer = setTimeout(() => { setRows([]); setCursor(undefined); void load() }, query ? 250 : 0)
    return () => { clearTimeout(timer); ++requestGeneration.current; requestController.current?.abort() }
  }, [load, query])
  useEffect(() => {
    const dirty = new Set<string>(); let timer: ReturnType<typeof setTimeout> | undefined; let active = true
    const off = subscribeEvents(['codex.session.updated'], (event) => {
      try { const { payload } = JSON.parse(event.data) as { payload?: { session_id?: string } }; if (payload?.session_id) dirty.add(payload.session_id) } catch { return }
      timer ??= setTimeout(() => {
        timer = undefined
        const ids = [...dirty]; dirty.clear()
        if (filtered) { void load(); return }
        // Fetch only affected metadata, in bounded batches; no 50-row refresh.
        void (async () => {
          for (let i = 0; i < ids.length; i += 4) await Promise.all(ids.slice(i, i + 4).map(async (id) => {
            try {
              const fresh = await json<CodexSession>(`/api/v1/codex/sessions/${encodeURIComponent(id)}`)
              if (!active) return
              setRows((old) => {
                const excluded = archived !== 'all' && (fresh.state === 'archived') !== (archived === 'true')
                if (excluded) return old.filter((s) => s.session_id !== id)
                const found = old.find((s) => s.session_id === id)
                return found ? old.map((s) => s.session_id === id ? { ...s, ...fresh } : s) : [fresh, ...old]
              })
            } catch { /* Current conversation reports its own refresh error. */ }
          }))
        })()
      }, 200)
    })
    return () => { active = false; if (timer) clearTimeout(timer); off() }
  }, [archived, filtered, load])
  return { rows, cursor, incomplete, loading, error, refresh: () => load(), more: () => load(cursor) }
}
