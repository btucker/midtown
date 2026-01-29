import { defineConfig } from '@playwright/test'

const webPort = process.env.MIDTOWN_WEB_PORT || 47022

export default defineConfig({
  testDir: './e2e',
  timeout: 30000,
  retries: 1,
  use: {
    baseURL: `http://localhost:${webPort}`,
    headless: true,
  },
  projects: [
    {
      name: 'chromium',
      use: { browserName: 'chromium' },
    },
  ],
  // When MIDTOWN_WEB_PORT is not set (no running daemon), start Vite dev server
  ...(!process.env.MIDTOWN_WEB_PORT && {
    webServer: {
      command: 'npm run dev -- --port 47022 --strictPort',
      port: 47022,
      reuseExistingServer: true,
    },
  }),
})
