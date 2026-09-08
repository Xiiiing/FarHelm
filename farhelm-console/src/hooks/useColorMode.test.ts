import { act, renderHook } from '@testing-library/react'
import { afterEach, expect, it } from 'vitest'
import { useColorMode } from './useColorMode'

afterEach(() => localStorage.clear())

it('does not overwrite a newer accent when another tab delivers the mode update first', () => {
  const { result } = renderHook(useColorMode)
  act(() => {
    localStorage.setItem('farhelm-color-mode', 'dark')
    localStorage.setItem('farhelm-accent', 'rose')
    window.dispatchEvent(new StorageEvent('storage', { key: 'farhelm-color-mode' }))
  })
  expect(result.current.mode).toBe('dark')
  expect(localStorage.getItem('farhelm-accent')).toBe('rose')
  act(() => { window.dispatchEvent(new StorageEvent('storage', { key: 'farhelm-accent' })) })
  expect(result.current.accent).toBe('rose')
})
