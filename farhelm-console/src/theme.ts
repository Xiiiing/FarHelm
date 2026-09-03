import { theme, type ThemeConfig } from 'antd'

export type ColorMode = 'light' | 'dark'

const common: ThemeConfig['token'] = {
  colorPrimary: '#22C7A9',
  colorSuccess: '#2DA44E',
  colorWarning: '#D97706',
  colorError: '#D14343',
  colorInfo: '#3B82F6',
  borderRadius: 8,
  controlHeight: 44,
  fontFamily:
    '-apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", sans-serif',
  fontFamilyCode:
    'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace',
  motionDurationFast: '0.12s',
  motionDurationMid: '0.16s',
  motionDurationSlow: '0.2s',
}

export function createTheme(mode: ColorMode): ThemeConfig {
  const dark = mode === 'dark'
  return {
    algorithm: dark ? theme.darkAlgorithm : theme.defaultAlgorithm,
    token: {
      ...common,
      colorPrimary: dark ? '#22C7A9' : '#087F6B',
      colorBgBase: dark ? '#0B0F14' : '#F4F7F9',
      colorBgContainer: dark ? '#111821' : '#FFFFFF',
      colorBgElevated: dark ? '#17212B' : '#EAF0F3',
      colorBorder: dark ? '#283441' : '#D4DEE5',
      colorText: dark ? '#F4F7FA' : '#17212B',
      colorTextSecondary: dark ? '#9AA8B6' : '#5F6F7D',
    },
    components: {
      Layout: {
        bodyBg: dark ? '#0B0F14' : '#F4F7F9',
        siderBg: dark ? '#111821' : '#FFFFFF',
      },
      Menu: {
        darkItemBg: '#111821',
        darkItemSelectedBg: '#172D2A',
        itemHeight: 44,
      },
      Card: { borderRadiusLG: 12 },
      Button: { borderRadius: 8 },
    },
  }
}
