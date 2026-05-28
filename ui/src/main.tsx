import React from 'react'
import ReactDOM from 'react-dom/client'
import App from './App'
import { ThemeProvider } from './hooks/useTheme'
import { applySettings, loadSettings } from './lib/a11y'
import './styles/tokens.css'
import '@xyflow/react/dist/style.css'

// Apply persisted a11y settings before first paint so font scale /
// contrast / motion toggles don't visibly flip after hydration.
applySettings(loadSettings())

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <ThemeProvider>
      <App />
    </ThemeProvider>
  </React.StrictMode>
)
