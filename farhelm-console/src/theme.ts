import { theme, type ThemeConfig } from 'antd'

export type ColorMode = 'light' | 'dark'

export const uiFont = '"Manrope Variable", "Noto Sans SC Variable", system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif'
export const palettes = {
  light: { canvas: '#F7F7F7', surface: '#FFFFFF', raised: '#F0F0F0', border: '#E3E3E3', text: '#181818', muted: '#666666', danger: '#B42318', selection: '#EAEAEA' },
  dark: { canvas: '#171717', surface: '#212121', raised: '#303030', border: '#414141', text: '#F5F5F5', muted: '#B5B5B5', danger: '#FF9A92', selection: '#383838' },
} as const

export const accentPresets = [
  { id: 'graphite', label: '石墨', color: '#555555' },
  { id: 'blue', label: '蓝色', color: '#2563EB' },
  { id: 'green', label: '绿色', color: '#00865A' },
  { id: 'violet', label: '紫色', color: '#8555D9' },
  { id: 'rose', label: '玫瑰', color: '#CF477A' },
  { id: 'orange', label: '橙色', color: '#C26819' },
] as const
export type AccentPreference = (typeof accentPresets)[number]['id'] | `#${string}`
export const defaultAccent: AccentPreference = 'blue'

export function parseAccent(value: unknown): AccentPreference | undefined {
  if (typeof value !== 'string') return undefined
  const candidate = value.trim()
  const preset = accentPresets.find(({ id }) => id === candidate)
  if (preset) return preset.id
  if (/^#[0-9a-f]{6}$/i.test(candidate)) return candidate.toUpperCase() as AccentPreference
  if (/^#[0-9a-f]{3}$/i.test(candidate)) return `#${candidate.slice(1).split('').map((digit) => digit + digit).join('').toUpperCase()}`
  return undefined
}

export function accentColor(preference: AccentPreference): string {
  return accentPresets.find(({ id }) => id === preference)?.color ?? parseAccent(preference) ?? '#2563EB'
}

function channels(hex: string): number[] {
  return [1, 3, 5].map((offset) => parseInt(hex.slice(offset, offset + 2), 16))
}
function luminance(hex: string): number {
  return channels(hex).map((channel) => channel / 255)
    .map((channel) => channel <= .04045 ? channel / 12.92 : ((channel + .055) / 1.055) ** 2.4)
    .reduce((sum, channel, index) => sum + channel * [.2126, .7152, .0722][index], 0)
}
export function contrastRatio(a: string, b: string): number {
  const x = luminance(a), y = luminance(b)
  return (Math.max(x, y) + .05) / (Math.min(x, y) + .05)
}

export function createPalette(mode: ColorMode, preference: AccentPreference = defaultAccent) {
  const base = palettes[mode]
  const source = preference === 'graphite' ? base.text : accentColor(preference)
  const backgrounds = [base.canvas, base.surface, base.raised, base.selection]
  let accent = source
  const target = mode === 'light' ? 0 : 255
  // Only appearance changes run this bounded adjustment; message rendering never does.
  for (let step = 0; step <= 100; step++) {
    accent = '#' + channels(source).map((channel) => Math.round(channel + (target - channel) * step / 100).toString(16).padStart(2, '0')).join('').toUpperCase()
    if (backgrounds.every((background) => contrastRatio(accent, background) >= 4.5)) break
  }
  const onAccent = contrastRatio(accent, '#FFFFFF') >= contrastRatio(accent, '#171717') ? '#FFFFFF' : '#171717'
  return { ...base, accent, 'on-accent': onAccent }
}

const common: ThemeConfig['token'] = {
  borderRadius: 14,
  borderRadiusLG: 20,
  borderRadiusSM: 10,
  controlHeight: 44,
  fontFamily: uiFont,
  fontFamilyCode:
    'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace',
  motionDurationFast: '0.12s',
  motionDurationMid: '0.18s',
  motionDurationSlow: '0.24s',
}

export function createTheme(mode: ColorMode, accent: AccentPreference = defaultAccent): ThemeConfig {
  const dark = mode === 'dark'
  const colors = createPalette(mode, accent)
  return {
    algorithm: dark ? theme.darkAlgorithm : theme.defaultAlgorithm,
    token: {
      ...common,
      colorPrimary: colors.accent,
      colorSuccess: colors.accent,
      colorInfo: colors.accent,
      colorWarning: colors.muted,
      colorLink: colors.accent,
      colorLinkHover: colors.accent,
      colorBgBase: colors.canvas,
      colorBgContainer: colors.surface,
      colorBgElevated: colors.raised,
      colorBorder: colors.border,
      colorText: colors.text,
      colorTextSecondary: colors.muted,
      colorTextDescription: colors.muted,
      colorTextPlaceholder: colors.muted,
      colorError: colors.danger,
    },
    components: {
      Layout: {
        bodyBg: colors.canvas,
        siderBg: colors.canvas,
      },
      Menu: {
        darkItemBg: palettes.dark.canvas,
        darkItemSelectedBg: palettes.dark.raised,
        itemHeight: 44,
        itemSelectedBg: colors.selection,
        itemSelectedColor: colors.text,
      },
      Card: { borderRadiusLG: 20 },
      Button: {
        borderRadius: 14,
        colorPrimary: colors.accent,
        colorPrimaryHover: colors.accent,
        colorPrimaryActive: colors.accent,
        primaryColor: colors['on-accent'],
      },
      Segmented: { itemSelectedBg: colors.surface, itemSelectedColor: colors.text, trackBg: colors.raised },
    },
  }
}
