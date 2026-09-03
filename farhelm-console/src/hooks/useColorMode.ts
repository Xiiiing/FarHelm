import { useEffect, useState } from 'react'

import type { ColorMode } from '../theme'

const STORAGE_KEY = 'farhelm-color-mode'

function preferredMode(): ColorMode {
  const saved = localStorage.getItem(STORAGE_KEY)
  if (saved === 'light' || saved === 'dark') return saved
  return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'
}

export function useColorMode() {
  const [mode, setMode] = useState<ColorMode>(preferredMode)

  useEffect(() => {
    document.documentElement.dataset.theme = mode
    document.documentElement.style.colorScheme = mode
    localStorage.setItem(STORAGE_KEY, mode)
  }, [mode])

  return {
    mode,
    toggleMode: () => setMode((current) => (current === 'dark' ? 'light' : 'dark')),
  }
}
