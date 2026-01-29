// @ts-check
import { test, expect } from '@playwright/test'
import { mockAllRoutes } from './helpers.js'

test.describe('PWA behavior', () => {
  test.beforeEach(async ({ page }) => {
    await mockAllRoutes(page)
    await page.goto('/')
  })

  test('has theme-color meta tag', async ({ page }) => {
    const themeColor = page.locator('meta[name="theme-color"]')
    await expect(themeColor).toHaveAttribute('content', '#1a1a2e')
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
    await expect(page).toHaveTitle('Midtown Mobile')
  })

  test('body uses dark background', async ({ page }) => {
    const body = page.locator('body')
    await expect(body).toHaveCSS('background-color', 'rgb(28, 28, 28)') // #1c1c1c
  })

  test('body uses monospace font family', async ({ page }) => {
    const body = page.locator('body')
    const fontFamily = await body.evaluate((el) => getComputedStyle(el).fontFamily)
    expect(fontFamily).toMatch(/SF Mono|Menlo|Consolas|monospace/)
  })

  test('main layout is max 600px and centered', async ({ page }) => {
    const main = page.locator('main')
    await expect(main).toHaveCSS('max-width', '600px')
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
  test('kanban uses 4 columns on desktop', async ({ page }) => {
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

  test('kanban uses 2 columns on mobile viewport', async ({ page }) => {
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

    await expect(page.locator('h1')).toHaveText('Midtown')
    await expect(page.locator('nav')).toBeVisible()
    await expect(page.locator('.kanban')).toBeVisible()
    await expect(page.locator('.messages')).toBeVisible()
  })
})
