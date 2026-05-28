/// <reference types="vitest" />
import { defineConfig, loadEnv } from 'vite'
import react from '@vitejs/plugin-react'
import os from 'node:os'

declare const process: { cwd(): string }

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), '')
  const apiPort = env.VITE_API_PORT ?? '3000'
  // Worker threads are much faster than the default forks pool for jsdom; cap at core count.
  const maxThreads = Math.max(1, os.cpus().length)
  return {
    plugins: [react()],
    server: {
      port: 5173,
      proxy: {
        '/api': { target: `http://localhost:${apiPort}`, changeOrigin: true },
      },
    },
    test: {
      environment: 'jsdom',
      globals: true,
      setupFiles: ['./src/test/setup.ts'],
      include: ['src/**/*.test.{ts,tsx}'],
      css: false,
      pool: 'threads',
      maxWorkers: maxThreads,
      isolate: true,
      fileParallelism: true,
    },
  }
})
