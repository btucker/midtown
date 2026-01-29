// @ts-check
import { test, expect } from '@playwright/test'
import { mockAllRoutes } from './helpers.js'

test.describe('Lead tab', () => {
  test.beforeEach(async ({ page }) => {
    await mockAllRoutes(page)
    await page.goto('/')
    await page.getByRole('button', { name: 'Lead' }).click()
    await expect(page.locator('.lead-container')).toBeVisible()
  })

  test('renders lead pane content', async ({ page }) => {
    const pane = page.locator('.pane-content')
    await expect(pane).toBeVisible()
    await expect(pane).toContainText('Running tests...')
    await expect(pane).toContainText('All 42 tests passed.')
  })

  test('pane content is displayed as preformatted text', async ({ page }) => {
    const pane = page.locator('.pane-content')
    // Should be a <pre> element
    await expect(pane).toBeVisible()
    const tagName = await pane.evaluate((el) => el.tagName.toLowerCase())
    expect(tagName).toBe('pre')
  })

  test('pane uses monospace font', async ({ page }) => {
    const pane = page.locator('.pane-content')
    const fontFamily = await pane.evaluate((el) => getComputedStyle(el).fontFamily)
    expect(fontFamily).toMatch(/SF Mono|Monaco|Menlo|Consolas|monospace/)
  })

  test('shows error banner when lead session not found', async ({ page }) => {
    await page.route('**/api/lead-pane', (route) =>
      route.fulfill({ status: 404, contentType: 'application/json', body: '{}' })
    )
    await page.goto('/')
    await page.getByRole('button', { name: 'Lead' }).click()

    const errorBanner = page.locator('.error-banner')
    await expect(errorBanner).toBeVisible()
    await expect(errorBanner).toContainText('Lead session not found')
  })

  test('shows error banner on connection failure', async ({ page }) => {
    await page.route('**/api/lead-pane', (route) => route.abort())
    await page.goto('/')
    await page.getByRole('button', { name: 'Lead' }).click()

    const errorBanner = page.locator('.error-banner')
    await expect(errorBanner).toBeVisible()
    await expect(errorBanner).toContainText('Failed to connect')
  })

  test('polls lead-pane endpoint periodically', async ({ page }) => {
    let fetchCount = 0
    await page.route('**/api/lead-pane', (route) => {
      fetchCount++
      route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ content: `Fetch #${fetchCount}` }),
      })
    })
    await page.goto('/')
    await page.getByRole('button', { name: 'Lead' }).click()

    // Wait for at least 2 poll cycles (1s interval)
    await page.waitForTimeout(2500)
    expect(fetchCount).toBeGreaterThanOrEqual(2)
  })

  test('lead container has dark background', async ({ page }) => {
    const container = page.locator('.lead-container')
    await expect(container).toHaveCSS('background-color', 'rgb(13, 13, 13)') // #0d0d0d
  })
})
