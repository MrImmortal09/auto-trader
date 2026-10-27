import { fileURLToPath } from 'node:url'
import { defineConfig, type Connect } from 'vite'
import react, { reactCompilerPreset } from '@vitejs/plugin-react'
import babel from '@rolldown/plugin-babel'
import tailwindcss from '@tailwindcss/vite'

// v1's screens are paths under /v1/ (/v1/settings): answer them with v1's
// page, as vercel.json does, instead of Vite's fallback to v2's.
const v1Fallback: Connect.NextHandleFunction = (req, _res, next) => {
  const path = req.url?.split('?')[0] ?? ''
  if (/^\/v1(\/[^.]*)?$/.test(path)) req.url = '/v1/index.html'
  next()
}

// https://vite.dev/config/
export default defineConfig({
  plugins: [
    tailwindcss(),
    react(),
    babel({ presets: [reactCompilerPreset()] }),
    {
      name: 'v1-fallback',
      configureServer: (server) => void server.middlewares.use(v1Fallback),
      configurePreviewServer: (server) => void server.middlewares.use(v1Fallback),
    },
  ],
  build: {
    rolldownOptions: {
      // Two pages: the redesign at / and the old app at /v1/ (emitted as
      // dist/v1/index.html), kept for rollback.
      input: {
        main: fileURLToPath(new URL('./index.html', import.meta.url)),
        v1: fileURLToPath(new URL('./v1/index.html', import.meta.url)),
      },
    },
  },
  server: {
    proxy: {
      // Forward all /api/* requests to the Rust backend during development.
      '/api': {
        target: 'http://127.0.0.1:8080',
        changeOrigin: true,
      },
    },
  },
})
