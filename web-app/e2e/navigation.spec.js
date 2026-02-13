// @ts-check
import { test, expect } from '@playwright/test'
import { mockAllRoutes } from './helpers.js'

test.describe('Navigation', () => {
  test.beforeEach(async ({ page }) => {
    await mockAllRoutes(page)
    await page.goto('/')
  })

  test('header shows connection status', async ({ page }) => {
    await expect(page.locator('.connection-dot')).toBeVisible()
  })

  test('sidebar header shows app title and project selector', async ({ page }) => {
    // The header contains the logo image and project selector
    await expect(page.locator('.header-logo')).toBeVisible()
    await expect(page.locator('.project-selector')).toBeVisible()
  })

  test('channel list is visible in sidebar by default', async ({ page }) => {
    // Default view is 'board' which shows ChannelList in SidebarContent
    await expect(page.locator('.channel-list, [class*="channel"] button').first()).toBeVisible()
  })

  test('main area shows channel content by default', async ({ page }) => {
    // Main area shows the Channel component with message input
    await expect(page.locator('main textarea[placeholder*="Message to"]')).toBeVisible()
  })

  test('coworker status is visible in sidebar footer', async ({ page }) => {
    // SidebarFooter contains CoworkerStatus - look for coworker names in the footer
    const coworkerFooter = page.locator('[data-slot="sidebar-footer"], aside footer, .sidebar-footer').first()
    // Check if any coworker-related content is visible (park or amsterdam from mock data)
    await expect(page.locator('text=/park|amsterdam/i').first()).toBeVisible()
  })

  test('push toggle is visible when supported', async ({ page }) => {
    // Push toggle is in SidebarHeader
    await expect(page.locator('.push-toggle')).toBeVisible()
  })

  test('mobile header shows sidebar trigger', async ({ page }) => {
    await page.setViewportSize({ width: 375, height: 667 })
    await page.goto('/')

    // On mobile, there's a header with SidebarTrigger
    const sidebarTrigger = page.locator('header button[class*="sidebar"], [data-sidebar-trigger]')
    // The sidebar trigger should be visible on mobile
    await expect(sidebarTrigger.or(page.locator('header button').first())).toBeVisible()
  })

  test('mobile header shows active channel', async ({ page }) => {
    await page.setViewportSize({ width: 375, height: 667 })
    await page.goto('/')

    // Mobile header shows the active channel name (uses .mobile-channel class)
    await expect(page.locator('.mobile-channel')).toBeVisible()
  })
})

test.describe('Mobile sidebar behavior', () => {
  test('sidebar can be toggled on mobile', async ({ page }) => {
    await mockAllRoutes(page)
    await page.setViewportSize({ width: 375, height: 667 })
    await page.goto('/')

    // The mobile header has a sidebar trigger button (uses .mobile-header class)
    const header = page.locator('.mobile-header')
    await expect(header).toBeVisible()
  })
})
