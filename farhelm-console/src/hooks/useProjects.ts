import { useMutation, useQuery } from '@tanstack/react-query'
import { ApiError, fetchProjects, fetchProjectPreferences, saveProjectPreferences, type ProjectCandidate, type ProjectPreference } from '../api/features'
import { queryClient } from '../api/cache'

export const projectKey = (project: { agent_id: string; suggested_project_id: string }) => JSON.stringify([project.agent_id, project.suggested_project_id])
export function useProjects(csrf: string) {
  const catalog = useQuery({ queryKey: ['projects'], queryFn: fetchProjects })
  const preferences = useQuery({ queryKey: ['project-preferences'], queryFn: fetchProjectPreferences })
  const preference = (p: ProjectCandidate): ProjectPreference => preferences.data?.projects.find((v) => v.agent_id === p.agent_id && v.project_id === p.suggested_project_id) ?? { agent_id: p.agent_id, project_id: p.suggested_project_id, display_name: null, hidden: false, pinned: false }
  const save = useMutation({
    mutationFn: (projects: ProjectPreference[]) => {
      const current = queryClient.getQueryData<typeof preferences.data>(['project-preferences'])
      if (!current) throw new Error('请先读取项目展示设置')
      return saveProjectPreferences(csrf, current.revision, projects)
    },
    onSuccess: (data) => { queryClient.setQueryData(['project-preferences'], data); void queryClient.invalidateQueries({ queryKey: ['codex', 'sessions'] }) },
    onError: (error) => { if (error instanceof ApiError && error.code === 'project_preferences_conflict') void preferences.refetch() },
  })
  const projects = (catalog.data ?? []).map((p) => ({ ...p, display_name: preference(p).display_name ?? p.display_name }))
  const approved = projects.filter((p) => p.state === 'approved').sort((a, b) => Number(preference(b).pinned) - Number(preference(a).pinned) || (b.last_activity_unix ?? 0) - (a.last_activity_unix ?? 0) || projectKey(a).localeCompare(projectKey(b)))
  return { catalog, preferences, projects, approved, visible: approved.filter((p) => !preference(p).hidden), preference, save }
}
