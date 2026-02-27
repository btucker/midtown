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
    await expect(page.locator('.project-name')).toBeVisible({ timeout: 5000 })

    // URL should now reflect the selected project
    await expect(page).toHaveURL(/\/test-project/)
  })

  test('URL updates when switching projects via the switcher', async ({ page }) => {
    await mockAllRoutes(page, { projects: TWO_PROJECTS })
    await page.goto('/')

    // Wait for app to initialize
    await expect(page.locator('.project-name')).toBeVisible({ timeout: 5000 })

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
    await expect(page.locator('.project-name')).toBeVisible({ timeout: 5000 })

    // The project name in the trigger should match the URL
    await expect(page.locator('.project-name')).toContainText('other-project')
  })

  test('falls back to first running project when URL project not found', async ({ page }) => {
    await mockAllRoutes(page)
    await page.goto('/nonexistent-project')

    // Wait for app to initialize
    await expect(page.locator('.project-name')).toBeVisible({ timeout: 5000 })

    // Should fall back to 'test-project'
    await expect(page.locator('.project-name')).toContainText('test-project')
  })

  test('URL-encodes project names with special characters', async ({ page }) => {
    const specialProjects = [
      { name: 'my project', status: 'running', webhook_port: 47099 },
    ]
    await mockAllRoutes(page, { projects: specialProjects })
    await page.goto('/')

    // Wait for app to initialize
    await expect(page.locator('.project-name')).toBeVisible({ timeout: 5000 })

    // URL should be percent-encoded
    await expect(page).toHaveURL(/\/my%20project/)

    // Navigating to the encoded URL should restore the correct project
    await page.goto('/my%20project')
    await expect(page.locator('.project-name')).toBeVisible({ timeout: 5000 })
    await expect(page.locator('.project-name')).toContainText('my project')
  })
})
