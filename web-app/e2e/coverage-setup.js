// @ts-check
/**
 * Playwright fixture that collects Istanbul code coverage from the browser
 * after each test. Requires COVERAGE=true when starting the Vite dev server
 * so that Istanbul instruments the source code.
 *
 * Usage:
 *   COVERAGE=true npm run dev -- --port 47111 &
 *   npx playwright test --config playwright.coverage.config.js
 *   npx nyc report --reporter=text --reporter=html
 */

import { test as base, expect } from '@playwright/test'
import { writeFileSync, mkdirSync, existsSync } from 'fs'
import { join } from 'path'

const COVERAGE_DIR = join(import.meta.dirname, '..', '.nyc_output')

// Ensure coverage output directory exists
if (!existsSync(COVERAGE_DIR)) {
  mkdirSync(COVERAGE_DIR, { recursive: true })
}

let coverageCounter = 0

/**
 * Extended test fixture that dumps coverage after each test.
 */
export const test = base.extend({
  page: async ({ page }, use) => {
    await use(page)

    // After the test, collect coverage from the page
    try {
      const coverage = await page.evaluate(() => {
        // @ts-ignore — Istanbul injects __coverage__ on window
        return window.__coverage__ || null
      })

      if (coverage) {
        const id = `coverage-${process.pid}-${++coverageCounter}.json`
        writeFileSync(join(COVERAGE_DIR, id), JSON.stringify(coverage))
      }
    } catch {
      // Page may have closed — ignore
    }
  },
})

export { expect }
