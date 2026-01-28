import { defineConfig } from '@playwright/test'

export default defineConfig({
  testDir: './e2e',
  timeout: 30000,
  retries: 1,
  use: {
    // Tests run against the daemon's built-in web server
    baseURL: `http://localhost:${process.env.MIDTOWN_WEB_PORT || 47022}`,
    headless: true,
  },
  projects: [
    {
      name: 'chromium',
      use: { browserName: 'chromium' },
    },
  ],
})
