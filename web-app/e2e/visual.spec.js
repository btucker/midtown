// @ts-check
import { test, expect } from '@playwright/test'
import path from 'node:path'
import fs from 'node:fs/promises'
import { fileURLToPath } from 'node:url'
import { mockAllRoutes, MOCK_STATUS } from './helpers.js'

const __filename = fileURLToPath(import.meta.url)
const __dirname = path.dirname(__filename)

const variant = process.env.SCREENSHOT_VARIANT ?? 'after'
const outDir = process.env.SCREENSHOT_OUT_DIR ?? path.resolve(__dirname, '../../screenshots')
const basename = process.env.SCREENSHOT_BASENAME ?? 'pr1499-dark'
const screenshotPath = path.join(outDir, `${basename}-${variant}.png`)

test('capture desktop channel view snapshot', async ({ page }) => {
  await fs.mkdir(outDir, { recursive: true })

  // Force dark theme for consistent comparison
  await page.addInitScript(() => {
    localStorage.setItem('midtown-theme', 'dark')
  })

  const statusPayload = JSON.parse(JSON.stringify(MOCK_STATUS))
  await mockAllRoutes(page, { status: statusPayload })

  await page.setViewportSize({ width: 1400, height: 900 })
  await page.goto('/')

  // Ensure the main channel area rendered before capturing
  await expect(page.locator('.channel-main')).toBeVisible()

  // Allow fonts/layout to settle for consistent diffs
  await page.waitForTimeout(500)

  await page.screenshot({ path: screenshotPath, fullPage: true })
})
