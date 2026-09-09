import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { BrowserRouter } from 'react-router-dom'
import { registerSW } from 'virtual:pwa-register'

import App from './App'
// Unicode ranges load only the local font slices needed by the current page.
import '@fontsource-variable/noto-sans-sc'
import './styles.css'
import { hasUnsavedDrafts } from './pwaUpdate'

const updateSW = registerSW({ immediate: true, onNeedRefresh: () => {
  if (!hasUnsavedDrafts() || window.confirm('新版 FarHelm 已就绪。当前有未发送草稿，仍要刷新吗？')) void updateSW(true)
} })

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <BrowserRouter useTransitions={false}>
      <App />
    </BrowserRouter>
  </StrictMode>,
)
