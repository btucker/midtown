// @ts-check
import { test, expect } from '@playwright/test'
import { loadApp } from './helpers.js'

test.describe('Theme toggle', () => {
  test('toggles and persists preference', async ({ page }) => {
    await loadApp(page)

    const html = page.locator('html')
    await expect(html).toHaveClass(/dark/)

    await page.getByTestId('theme-toggle').click()
    await expect(html).not.toHaveClass(/dark/)

    const stored = await page.evaluate(() => localStorage.getItem('midtown-theme'))
    expect(stored).toBe('light')

    await page.reload()
    await expect(html).not.toHaveClass(/dark/)

    await page.getByTestId('theme-toggle').click()
    await expect(html).toHaveClass(/dark/)
  })
})
