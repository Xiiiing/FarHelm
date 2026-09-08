import { theme, type ThemeConfig } from 'antd'

export type ColorMode = 'light' | 'dark'

export const uiFont = 'system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", "Noto Sans SC", "Noto Sans CJK SC", "PingFang SC", "Microsoft YaHei", sans-serif'
export const palettes = {
  light: { canvas: '#F3F4F5', surface: '#FFFFFF', raised: '#ECEEF0', border: '#E1E4E8', text: '#20242B', muted: '#646B75', accent: '#087B68', danger: '#D14343' },
  dark: { canvas: '#101214', surface: '#181B1E', raised: '#22262B', border: '#30353A', text: '#ECEEF0', muted: '#A0A7AF', accent: '#22C7A9', danger: '#D14343' },
} as const

const common: ThemeConfig['token'] = {
  colorPrimary: '#22C7A9',
  colorSuccess: '#2DA44E',
  colorWarning: '#D97706',
  colorError: '#D14343',
  colorInfo: '#3B82F6',
  borderRadius: 8,
  controlHeight: 44,
  fontFamily: uiFont,
  fontFamilyCode:
    'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace',
  motionDurationFast: '0.12s',
  motionDurationMid: '0.16s',
  motionDurationSlow: '0.2s',
}

export function createTheme(mode: ColorMode): ThemeConfig {
  const dark = mode === 'dark'
  const colors = palettes[mode]
  return {
    algorithm: dark ? theme.darkAlgorithm : theme.defaultAlgorithm,
    token: {
      ...common,
      colorPrimary: colors.accent,
      colorBgBase: colors.canvas,
      colorBgContainer: colors.surface,
      colorBgElevated: colors.raised,
      colorBorder: colors.border,
      colorText: colors.text,
      colorTextSecondary: colors.muted,
      colorTextPlaceholder: colors.muted,
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
      },
      Card: { borderRadiusLG: 12 },
      Button: { borderRadius: 8 },
    },
  }
}
