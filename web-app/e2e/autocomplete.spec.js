import { test, expect } from '@playwright/test'
import { mockAllRoutes } from './helpers.js'

test.describe('Autocomplete', () => {
  test.beforeEach(async ({ page }) => {
    // Set up mocked API routes
    await mockAllRoutes(page)
    // Navigate to the web UI
    await page.goto('/')
    // Wait for the channel container to load
    await expect(page.locator('.channel-container')).toBeVisible()
  })

  test('should maintain autocomplete dropdown when typing more characters for @mention', async ({ page }) => {
    // Focus the input textarea
    const textarea = page.locator('textarea[placeholder*="Message to"]')
    await textarea.click()

    // Type '@m' and verify autocomplete appears
    await textarea.type('@m', { delay: 50 })
    await page.waitForSelector('.autocomplete-dropdown', { timeout: 1000 })

    // Verify autocomplete is visible
    let dropdown = page.locator('.autocomplete-dropdown')
    await expect(dropdown).toBeVisible()

    // Type 'a' to make it '@ma'
    await textarea.type('a', { delay: 50 })

    // Autocomplete should still be visible
    await expect(dropdown).toBeVisible()

    // Type 'd' to make it '@mad'
    await textarea.type('d', { delay: 50 })

    // Autocomplete should STILL be visible (this was the bug)
    await expect(dropdown).toBeVisible()

    // Verify 'madison' is in the results
    const items = page.locator('.autocomplete-item .item-label')
    await expect(items).toContainText('@madison')
  })

  test('should filter autocomplete results as more characters are typed', async ({ page }) => {
    const textarea = page.locator('textarea[placeholder*="Message to"]')
    await textarea.click()

    // Type '@m' - should show multiple results (madison, amsterdam, etc.)
    await textarea.type('@m', { delay: 50 })
    await page.waitForSelector('.autocomplete-dropdown', { timeout: 1000 })

    let items = page.locator('.autocomplete-item')
    const initialCount = await items.count()
    expect(initialCount).toBeGreaterThanOrEqual(2) // At least madison and amsterdam

    // Type 'ad' to make it '@mad' - should show only madison
    await textarea.type('ad', { delay: 50 })

    items = page.locator('.autocomplete-item')
    const filteredCount = await items.count()
    expect(filteredCount).toBe(1)

    // Verify it's madison
    const label = page.locator('.autocomplete-item .item-label')
    await expect(label).toContainText('@madison')
  })

  test('should hide autocomplete when typing a space after @ trigger', async ({ page }) => {
    const textarea = page.locator('textarea[placeholder*="Message to"]')
    await textarea.click()

    // Type '@m'
    await textarea.type('@m', { delay: 50 })
    await page.waitForSelector('.autocomplete-dropdown', { timeout: 1000 })

    // Type space
    await textarea.press('Space')

    // Autocomplete should hide
    const dropdown = page.locator('.autocomplete-dropdown')
    await expect(dropdown).not.toBeVisible()
  })

  test('should complete @mention on Tab key', async ({ page }) => {
    const textarea = page.locator('textarea[placeholder*="Message to"]')
    await textarea.click()

    // Type '@mad'
    await textarea.type('@mad', { delay: 50 })
    await page.waitForSelector('.autocomplete-dropdown', { timeout: 1000 })

    // Press Tab to complete
    await textarea.press('Tab')

    // Verify the textarea contains '@madison '
    const value = await textarea.inputValue()
    expect(value).toBe('@madison ')

    // Autocomplete should be hidden
    const dropdown = page.locator('.autocomplete-dropdown')
    await expect(dropdown).not.toBeVisible()
  })
})
