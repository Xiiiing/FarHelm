import { describe, expect, it } from 'vitest'
import { accentPresets, contrastRatio, createPalette, parseAccent } from './theme'

describe('appearance colors', () => {
  it('accepts only known presets and normalized hex values', () => {
    expect(parseAccent('blue')).toBe('blue')
    expect(parseAccent(' #3aF ')).toBe('#33AAFF')
    expect(parseAccent('#abcdef')).toBe('#ABCDEF')
    for (const value of [null, {}, 'unknown', 'red', '#12', '#12345g', 'url(https://example.test)', '#fff; color:red']) expect(parseAccent(value)).toBeUndefined()
  })

  it('keeps accents readable on every surface in both appearances, including extreme custom colors', () => {
    expect(contrastRatio('#000000', '#FFFFFF')).toBe(21)
    for (const mode of ['light', 'dark'] as const) {
      for (const choice of [...accentPresets.map(({ id }) => id), '#FFFFFF', '#000000', '#FFFF00', '#777777', '#FF00FF', '#00FFFF'] as const) {
        const colors = createPalette(mode, choice)
        for (const background of [colors.canvas, colors.surface, colors.raised, colors.selection, colors['accent-soft']]) {
          expect(contrastRatio(colors.accent, background), `${mode}/${choice}/${background}`).toBeGreaterThanOrEqual(4.5)
          expect(contrastRatio(colors.text, background)).toBeGreaterThanOrEqual(4.5)
          expect(contrastRatio(colors.muted, background)).toBeGreaterThanOrEqual(4.5)
        }
        expect(contrastRatio(colors.accent, colors['on-accent'])).toBeGreaterThanOrEqual(4.5)
        // Reading surfaces stay neutral; interactive selection uses the chosen accent.
        expect(colors.surface).toBe(createPalette(mode, 'graphite').surface)
        expect(colors.selection).toBe(createPalette(mode, 'graphite').selection)
        expect(colors.danger).toBe(colors.text)
      }
    }
  })
})
