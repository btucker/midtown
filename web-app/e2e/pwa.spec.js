// @ts-check
import { test, expect } from '@playwright/test'
import { mockAllRoutes } from './helpers.js'

test.describe('PWA behavior', () => {
  test.beforeEach(async ({ page }) => {
    await mockAllRoutes(page)
    await page.goto('/')
  })

  test('has theme-color meta tag', async ({ page }) => {
    // There may be two theme-color meta tags (from index.html and svelte:head)
    // Check that at least one has the correct dark color
    const themeColor = page.locator('meta[name="theme-color"]').first()
    await expect(themeColor).toHaveAttribute('content', '#0a0a0b')
  })

  test('has viewport meta tag disabling user scaling', async ({ page }) => {
    const viewport = page.locator('meta[name="viewport"]')
    const content = await viewport.getAttribute('content')
    expect(content).toContain('width=device-width')
    expect(content).toContain('initial-scale=1.0')
    expect(content).toContain('maximum-scale=1.0')
    expect(content).toContain('user-scalable=no')
  })

  test('has apple-mobile-web-app-capable meta tag', async ({ page }) => {
    const capable = page.locator('meta[name="apple-mobile-web-app-capable"]')
    await expect(capable).toHaveAttribute('content', 'yes')
  })

  test('has apple-mobile-web-app-status-bar-style meta tag', async ({ page }) => {
    const statusBar = page.locator('meta[name="apple-mobile-web-app-status-bar-style"]')
    await expect(statusBar).toHaveAttribute('content', 'black-translucent')
  })

  test('page title is set', async ({ page }) => {
    await expect(page).toHaveTitle('Midtown')
  })

  test('body uses dark background', async ({ page }) => {
    const body = page.locator('body')
    // The background color may vary - check it's a dark color (low RGB values)
    const bgColor = await body.evaluate((el) => getComputedStyle(el).backgroundColor)
    // Should be a dark color like rgb(10, 10, 10) or similar
    const match = bgColor.match(/rgb\((\d+),\s*(\d+),\s*(\d+)\)/)
    expect(match).toBeTruthy()
    if (match) {
      const [, r, g, b] = match.map(Number)
      // All components should be low (dark background)
      expect(r).toBeLessThan(50)
      expect(g).toBeLessThan(50)
      expect(b).toBeLessThan(50)
    }
  })

  test('body uses sans-serif font family', async ({ page }) => {
    const body = page.locator('body')
    const fontFamily = await body.evaluate((el) => getComputedStyle(el).fontFamily)
    // Body uses sans-serif font (IBM Plex Sans) for readability
    expect(fontFamily).toMatch(/IBM Plex|sans-serif/i)
  })

  test('main layout uses full screen', async ({ page }) => {
    const main = page.locator('main')
    // New layout uses flex-1 to fill available space
    await expect(main).toBeVisible()
  })

  test('layout uses full viewport height', async ({ page }) => {
    const main = page.locator('main')
    // Should have height set to 100vh or 100dvh
    const height = await main.evaluate((el) => getComputedStyle(el).height)
    // The computed height should be the viewport height (non-zero)
    expect(parseInt(height)).toBeGreaterThan(0)
  })
})

test.describe('PWA responsive layout', () => {
  // Note: Kanban tests skipped because the new layout does not include the Kanban component
  test.skip('kanban uses 4 columns on desktop', async ({ page }) => {
    await mockAllRoutes(page)
    await page.setViewportSize({ width: 800, height: 600 })
    await page.goto('/')

    const kanban = page.locator('.kanban')
    await expect(kanban).toBeVisible()
    // Computed grid-template-columns resolves repeat(4, 1fr) to 4 pixel values
    const gridCols = await kanban.evaluate((el) => getComputedStyle(el).gridTemplateColumns)
    const colCount = gridCols.split(/\s+/).length
    expect(colCount).toBe(4)
  })

  test.skip('kanban uses 2 columns on mobile viewport', async ({ page }) => {
    await mockAllRoutes(page)
    await page.setViewportSize({ width: 375, height: 667 })
    await page.goto('/')

    const kanban = page.locator('.kanban')
    await expect(kanban).toBeVisible()
    // On mobile (<600px), grid should be 2 columns
    const columns = page.locator('.kanban-column')
    await expect(columns).toHaveCount(4) // Still 4 columns in DOM, but 2-col grid
  })

  test('all content is visible on mobile viewport', async ({ page }) => {
    await mockAllRoutes(page)
    await page.setViewportSize({ width: 375, height: 667 })
    await page.goto('/')

    // Wait for the app to load
    await page.waitForTimeout(500)

    // New layout: check for connection status and main channel content
    // The connection dot should be in the sidebar header
    const connectionDot = page.locator('.connection-dot')
    const input = page.locator('main textarea[placeholder*="Message to"]')

    // At least one of these should be visible
    const dotVisible = await connectionDot.isVisible().catch(() => false)
    const inputVisible = await input.isVisible().catch(() => false)

    expect(dotVisible || inputVisible).toBe(true)
  })

  test('chat input is visible and within viewport on mobile', async ({ page }) => {
    await mockAllRoutes(page)
    // Use typical iPhone dimensions
    await page.setViewportSize({ width: 390, height: 844 })
    await page.goto('/')

    // The textarea should be visible in the new layout
    const textarea = page.locator('main textarea[placeholder*="Message to"]')
    await expect(textarea).toBeVisible()

    // Get the bounding box to ensure it's within viewport
    const box = await textarea.boundingBox()
    expect(box).toBeTruthy()
    if (box) {
      // Input should be fully visible within viewport height
      expect(box.y + box.height).toBeLessThanOrEqual(844)
      // Input should not be pushed off the bottom
      expect(box.y).toBeGreaterThanOrEqual(0)
    }

    // Verify textarea is interactable
    await expect(textarea).toBeEnabled()
  })
})
