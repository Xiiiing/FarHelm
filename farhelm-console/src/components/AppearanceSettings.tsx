import { ArrowUpOutlined } from '@ant-design/icons'
import { Button, Card, ColorPicker, Input, Segmented } from 'antd'
import { useState } from 'react'
import type { ColorPreference } from '../hooks/useColorMode'
import { accentColor, accentPresets, parseAccent, type AccentPreference } from '../theme'
import './appearance.css'

export interface AppearanceProps {
  preference: ColorPreference
  onPreference: (value: ColorPreference) => void
  accent: AccentPreference
  onAccent: (value: AccentPreference) => void
}

function CustomColor({ accent, onAccent }: Pick<AppearanceProps, 'accent' | 'onAccent'>) {
  const [hex, setHex] = useState(accentColor(accent))
  const parsed = parseAccent(hex)
  const valid = parsed?.startsWith('#') === true
  return <div className="custom-accent">
    <label htmlFor="accent-hex">自定义颜色</label>
    <div className="custom-accent-controls">
      <ColorPicker value={accentColor(accent)} disabledAlpha format="hex" onChangeComplete={(color) => onAccent(color.toHexString().toUpperCase() as AccentPreference)}>
        <Button className="custom-accent-picker" aria-label="打开自定义取色器"><span style={{ backgroundColor: accentColor(accent) }} aria-hidden /></Button>
      </ColorPicker>
      <Input id="accent-hex" aria-describedby="accent-help" aria-invalid={!valid} value={hex} maxLength={7} spellCheck={false} status={valid ? undefined : 'error'} onChange={(event) => setHex(event.target.value)} onPressEnter={() => { if (valid && parsed) onAccent(parsed) }} />
      <Button className="apply-accent" disabled={!valid} onClick={() => { if (valid && parsed) onAccent(parsed) }}>应用颜色</Button>
    </div>
    <p id="accent-help" className={valid ? 'appearance-help' : 'appearance-help appearance-error'}>{valid ? '颜色会适配浅深模式，保持文字清晰。' : '请输入十六进制颜色，例如 #2563EB。'}</p>
  </div>
}

export function AppearanceSettings({ preference, onPreference, accent, onAccent }: AppearanceProps) {
  return <Card className="appearance-card" title="外观" extra={<span className="appearance-local">仅此浏览器</span>}>
    <div className="appearance-layout">
      <div className="appearance-controls">
        <div className="appearance-field"><span id="appearance-mode-label">显示模式</span><Segmented block aria-labelledby="appearance-mode-label" value={preference} onChange={onPreference} options={[{ value: 'system', label: '跟随系统' }, { value: 'light', label: '浅色' }, { value: 'dark', label: '深色' }]} /></div>
        <div className="appearance-field"><span id="accent-label">强调色</span><div className="accent-choices" role="group" aria-labelledby="accent-label">
          {accentPresets.map((preset) => <Button key={preset.id} type="text" className="accent-choice" aria-pressed={accent === preset.id} onClick={() => onAccent(preset.id)}><span className="accent-swatch" style={{ backgroundColor: preset.color }} aria-hidden /><span>{preset.label}</span></Button>)}
        </div></div>
        <CustomColor key={accent} accent={accent} onAccent={onAccent} />
      </div>
      <div className="appearance-preview" role="img" aria-label="当前外观预览：中性对话背景与所选强调色">
        <span className="appearance-preview-label">预览</span>
        <div className="preview-user">检查一下训练结果</div>
        <div className="preview-response"><strong>Codex</strong><p>训练已完成。我们来看看结果。</p><span className="preview-link">查看实验详情 →</span></div>
        <div className="preview-composer"><span>继续提问…</span><span className="preview-send"><ArrowUpOutlined aria-hidden /></span></div>
      </div>
    </div>
  </Card>
}
