import { useEffect, useState } from 'react'
import { createPalette, defaultAccent, parseAccent, uiFont, type AccentPreference, type ColorMode } from '../theme'
export type ColorPreference = ColorMode | 'system'
const STORAGE_KEY = 'farhelm-color-mode'
const ACCENT_KEY = 'farhelm-accent'
function read(key: string): string | null { try { return localStorage.getItem(key) } catch { return null } }
function save(key: string, value: string) { try { localStorage.setItem(key, value) } catch { /* Appearance still works when browser storage is disabled. */ } }
function parsePreference(saved: string | null): ColorPreference { return saved === 'light' || saved === 'dark' ? saved : 'system' }
export function useColorMode() {
  const [preference, setPreferenceState] = useState<ColorPreference>(() => parsePreference(read(STORAGE_KEY)))
  const [accent, setAccentState] = useState<AccentPreference>(() => parseAccent(read(ACCENT_KEY)) ?? defaultAccent)
  // Only a local user action writes its own key; a tab receiving an update never writes stale sibling preferences back.
  const setPreference = (value: ColorPreference) => { save(STORAGE_KEY, value); setPreferenceState(value) }
  const setAccent = (value: AccentPreference) => { save(ACCENT_KEY, value); setAccentState(value) }
  const [system, setSystem] = useState<ColorMode>(() => matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light')
  const mode = preference === 'system' ? system : preference
  useEffect(() => {
    const media = matchMedia('(prefers-color-scheme: dark)'); const change = () => setSystem(media.matches ? 'dark' : 'light')
    media.addEventListener('change', change)
    return () => media.removeEventListener('change', change)
  }, [])
  useEffect(() => {
    const sync = (event: StorageEvent) => {
      if (event.key === STORAGE_KEY || event.key === null) setPreferenceState(parsePreference(read(STORAGE_KEY)))
      if (event.key === ACCENT_KEY || event.key === null) setAccentState(parseAccent(read(ACCENT_KEY)) ?? defaultAccent)
    }
    window.addEventListener('storage', sync)
    return () => window.removeEventListener('storage', sync)
  }, [])
  useEffect(() => {
    const root = document.documentElement
    root.dataset.theme = mode; root.style.colorScheme = mode
    for (const [name, value] of Object.entries(createPalette(mode, accent))) root.style.setProperty(`--farhelm-${name}`, value)
    root.style.setProperty('--font-ui', uiFont)
  }, [mode, accent])
  return { mode, preference, setPreference, accent, setAccent, toggleMode: () => setPreference(mode === 'dark' ? 'light' : 'dark') }
}
