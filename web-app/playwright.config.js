import { defineConfig } from '@playwright/test'

const defaultWebPort = 47111
const webPort = process.env.MIDTOWN_WEB_PORT || defaultWebPort

// Use preview server for SW tests (PWA disabled in dev mode)
const isSwTest = process.env.TEST_SW === '1'

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
  // When MIDTOWN_WEB_PORT is not set, start a local server
  // Use preview server for SW tests (requires 'npm run build' first)
  // Use dev server for other tests
  ...(!process.env.MIDTOWN_WEB_PORT && {
    webServer: {
      command: isSwTest
        ? `npm run preview -- --port ${defaultWebPort} --strictPort`
        : `npm run dev -- --port ${defaultWebPort} --strictPort`,
      port: defaultWebPort,
      reuseExistingServer: true,
    },
  }),
})
