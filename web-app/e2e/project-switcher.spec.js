// @ts-check
import { test, expect } from '@playwright/test'
import { mockAllRoutes } from './helpers.js'

const TWO_PROJECTS = [
  { name: 'test-project', status: 'running', webhook_port: 47099 },
  { name: 'other-project', status: 'running', webhook_port: 47100 },
]

test.describe('Project switcher', () => {
  test('checkmark renders as ✓ character, not as literal \\u2713', async ({ page }) => {
    await mockAllRoutes(page)
    await page.goto('/')

    // Open project dropdown
    await page.locator('.project-trigger').click()

    // Active check span should render the actual checkmark character
    const checkmark = page.locator('.active-check')
    await expect(checkmark).toBeVisible()

    const text = await checkmark.textContent()
    expect(text?.trim()).toBe('✓')
    expect(text).not.toContain('u2713')
  })

  test('URL updates to /{project} on initial load', async ({ page }) => {
    await mockAllRoutes(page)
    await page.goto('/')

    // Wait for app to fully initialize
    await page.waitForSelector('.project-name', { timeout: 5000 })

    // URL should now reflect the selected project
    await expect(page).toHaveURL(/\/test-project/)
  })

  test('URL updates when switching projects via the switcher', async ({ page }) => {
    await mockAllRoutes(page, { projects: TWO_PROJECTS })
    await page.goto('/')

    // Wait for app to initialize
    await page.waitForSelector('.project-name', { timeout: 5000 })

    // Open dropdown and select other-project
    await page.locator('.project-trigger').click()
    await page.locator('.project-option:has-text("other-project")').click()

    // URL should reflect the new project
    await expect(page).toHaveURL(/\/other-project/)
  })

  test('loads the project from URL path on page load', async ({ page }) => {
    await mockAllRoutes(page, { projects: TWO_PROJECTS })
    await page.goto('/other-project')

    // Wait for app to initialize
    await page.waitForSelector('.project-name', { timeout: 5000 })

    // The project name in the trigger should match the URL
    const projectName = page.locator('.project-name')
    await expect(projectName).toContainText('other-project')
  })

  test('falls back to first running project when URL project not found', async ({ page }) => {
    await mockAllRoutes(page)
    await page.goto('/nonexistent-project')

    // Wait for app to initialize
    await page.waitForSelector('.project-name', { timeout: 5000 })

    // Should fall back to 'test-project'
    const projectName = page.locator('.project-name')
    await expect(projectName).toContainText('test-project')
  })
})
