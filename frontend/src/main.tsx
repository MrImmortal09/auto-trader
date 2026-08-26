import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import './index.css'
import App from './App.tsx'

import { AuthGuard } from './components/AuthGuard'

// A focused <input type="number"> treats the mouse wheel as increment/decrement,
// so scrolling the page past one silently changes it. Nothing here relies on
// wheel-to-nudge — every value is typed — so swallow the wheel for a focused
// number input. Focus is kept; move the pointer off the field and the page
// scrolls normally again.
document.addEventListener(
  'wheel',
  (e) => {
    const el = e.target as HTMLElement | null
    if (el instanceof HTMLInputElement && el.type === 'number' && el === document.activeElement) {
      e.preventDefault()
    }
  },
  { passive: false },
)

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <AuthGuard>
      <App />
    </AuthGuard>
  </StrictMode>,
)
