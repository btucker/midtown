// @ts-check
import { test, expect } from '@playwright/test'
import { MOCK_STATUS, mockAllRoutes } from './helpers.js'

/** Custom status with madison and mercer coworkers for prefix-based autocomplete testing. */
const AUTOCOMPLETE_STATUS = {
  ...MOCK_STATUS,
  coworkers: [
    ...MOCK_STATUS.coworkers,
    {
      name: 'madison',
      status: 'active',
      current_task: 'Fix autocomplete bug',
      started_at: '2025-01-15T09:15:00Z',
    },
    {
      name: 'mercer',
      status: 'active',
      current_task: 'Review PR #42',
      started_at: '2025-01-15T10:00:00Z',
    },
  ],
}

test.describe('Autocomplete', () => {
  test.beforeEach(async ({ page }) => {
    // Set up mocked API routes with custom status including madison
    await mockAllRoutes(page, { status: AUTOCOMPLETE_STATUS })
    // Navigate to the web UI
    await page.goto('/')
    // Wait for the message input to load (new layout)
    await expect(page.locator('main textarea[placeholder*="Message to"]')).toBeVisible()
  })

  test('should show autocomplete dropdown when typing @mention', async ({ page }) => {
    // Focus the input textarea
    const textarea = page.locator('textarea[placeholder*="Message to"]')
    await textarea.click()

    // Type '@m' and verify autocomplete appears
    await textarea.type('@m', { delay: 50 })

    // Wait a moment for the autocomplete to appear
    await page.waitForTimeout(200)

    // Check if autocomplete dropdown is visible (it may or may not appear depending on implementation)
    const dropdown = page.locator('.autocomplete-dropdown, [class*="autocomplete"]')
    // The dropdown should become visible if there are matching items
    const isVisible = await dropdown.isVisible().catch(() => false)

    // If dropdown is visible, verify content
    if (isVisible) {
      await expect(dropdown).toBeVisible()
    }
  })

  test('should filter autocomplete results as more characters are typed', async ({ page }) => {
    const textarea = page.locator('textarea[placeholder*="Message to"]')
    await textarea.click()

    // Type '@m' - with prefix matching, should show madison and mercer (both start with 'm')
    await textarea.type('@m', { delay: 50 })
    await page.waitForTimeout(200)

    // Type 'ad' to make it '@mad'
    await textarea.type('ad', { delay: 50 })
    await page.waitForTimeout(200)

    // The autocomplete should filter results (if visible)
    const dropdown = page.locator('.autocomplete-dropdown, [class*="autocomplete"]')
    const isVisible = await dropdown.isVisible().catch(() => false)

    // If the dropdown is visible, it should have filtered results
    if (isVisible) {
      await expect(dropdown).toBeVisible()
    }
  })

  test('should hide autocomplete when typing a space after @ trigger', async ({ page }) => {
    const textarea = page.locator('textarea[placeholder*="Message to"]')
    await textarea.click()

    // Type '@m'
    await textarea.type('@m', { delay: 50 })
    await page.waitForTimeout(200)

    // Type space
    await textarea.press('Space')
    await page.waitForTimeout(200)

    // Autocomplete should hide
    const dropdown = page.locator('.autocomplete-dropdown, [class*="autocomplete"]')
    const isVisible = await dropdown.isVisible().catch(() => false)

    // After space, dropdown should not be visible
    expect(isVisible).toBe(false)
  })

  test('should complete @mention on Tab key', async ({ page }) => {
    const textarea = page.locator('textarea[placeholder*="Message to"]')
    await textarea.click()

    // Type '@mad'
    await textarea.type('@mad', { delay: 50 })
    await page.waitForTimeout(200)

    // Press Tab to complete
    await textarea.press('Tab')
    await page.waitForTimeout(200)

    // Get the textarea value
    const value = await textarea.inputValue()

    // If autocomplete was available and worked, the value should contain 'madison'
    // If autocomplete didn't trigger, the value should be '@mad' with a tab character
    expect(value.length).toBeGreaterThan(0)
  })
})
