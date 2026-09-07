import { useEffect, useState } from 'react'
import type { ColorMode } from '../theme'
export type ColorPreference = ColorMode | 'system'
const STORAGE_KEY = 'farhelm-color-mode'
function readPreference(): ColorPreference { const saved = localStorage.getItem(STORAGE_KEY); return saved === 'light' || saved === 'dark' ? saved : 'system' }
export function useColorMode() {
  const [preference, setPreference] = useState<ColorPreference>(readPreference)
  const [system, setSystem] = useState<ColorMode>(() => matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light')
  const mode = preference === 'system' ? system : preference
  useEffect(() => {
    const media = matchMedia('(prefers-color-scheme: dark)'); const change = () => setSystem(media.matches ? 'dark' : 'light')
    media.addEventListener('change', change)
    return () => media.removeEventListener('change', change)
  }, [])
  useEffect(() => { document.documentElement.dataset.theme = mode; document.documentElement.style.colorScheme = mode; localStorage.setItem(STORAGE_KEY, preference) }, [mode, preference])
  return { mode, preference, setPreference, toggleMode: () => setPreference(mode === 'dark' ? 'light' : 'dark') }
}
