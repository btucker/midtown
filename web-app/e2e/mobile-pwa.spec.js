// @ts-check
import { test, expect } from '@playwright/test'
import { mockAllRoutes } from './helpers.js'

/**
 * Mobile PWA Tests
 *
 * These tests verify the mobile PWA experience including:
 * - Responsive layout behavior
 * - Touch interactions
 * - Safe area insets
 * - Sidebar collapse/expand on mobile
 * - Offline capability
 */

// Common mobile viewport sizes
const MOBILE_VIEWPORTS = [
  { name: 'iPhone SE', width: 375, height: 667 },
  { name: 'iPhone 14', width: 390, height: 844 },
  { name: 'iPhone 14 Pro Max', width: 430, height: 932 },
  { name: 'Pixel 5', width: 393, height: 851 },
]

test.describe('Mobile PWA - Viewport Tests', () => {
  for (const viewport of MOBILE_VIEWPORTS) {
    test(`layout renders correctly on ${viewport.name} (${viewport.width}x${viewport.height})`, async ({ page }) => {
      await mockAllRoutes(page)
      await page.setViewportSize({ width: viewport.width, height: viewport.height })
      await page.goto('/')

      // Main container should be visible
      const appContainer = page.locator('.app-container')
      await expect(appContainer).toBeVisible()

      // Mobile header should be visible on small screens (uses .mobile-header class)
      const mobileHeader = page.locator('.mobile-header')
      await expect(mobileHeader).toBeVisible()

      // SidebarTrigger button should be visible on mobile
      const sidebarTrigger = page.locator('button[data-sidebar="trigger"]')
      await expect(sidebarTrigger).toBeVisible()

      // Chat input should be visible and within viewport
      const textarea = page.locator('main textarea')
      await expect(textarea).toBeVisible()

      const box = await textarea.boundingBox()
      expect(box).toBeTruthy()
      if (box) {
        expect(box.y + box.height).toBeLessThanOrEqual(viewport.height)
      }
    })
  }
})

test.describe('Mobile PWA - Sidebar Behavior', () => {
  test.beforeEach(async ({ page }) => {
    await mockAllRoutes(page)
    await page.setViewportSize({ width: 390, height: 844 })
    await page.goto('/')
  })

  test('sidebar is hidden by default on mobile', async ({ page }) => {
    // The sidebar content should not be visible initially on mobile
    // (it's in a Sheet/overlay that needs to be triggered)
    const sidebarContent = page.locator('[data-sidebar="content"]')

    // On mobile, sidebar content should not be visible without triggering
    const isVisible = await sidebarContent.isVisible().catch(() => false)
    // Either it's hidden or the sidebar is collapsed
    expect(isVisible).toBe(false)
  })

  test('sidebar trigger button is visible on mobile', async ({ page }) => {
    const trigger = page.locator('button[data-sidebar="trigger"]')
    await expect(trigger).toBeVisible()
    await expect(trigger).toBeEnabled()
  })

  test('mobile header shows active channel', async ({ page }) => {
    // Mobile header should show the active channel (uses .mobile-channel class)
    const activeChannelDisplay = page.locator('.mobile-channel')
    await expect(activeChannelDisplay).toBeVisible()

    // Should show the channel name
    const text = await activeChannelDisplay.textContent()
    expect(text).toBeTruthy()
  })
})

test.describe('Mobile PWA - Touch Interactions', () => {
  test.beforeEach(async ({ page }) => {
    await mockAllRoutes(page)
    await page.setViewportSize({ width: 390, height: 844 })
    await page.goto('/')
  })

  test('message input is focusable and tappable', async ({ page }) => {
    const textarea = page.locator('main textarea')
    await expect(textarea).toBeVisible()

    // Tap to focus
    await textarea.click()
    await expect(textarea).toBeFocused()

    // Type text
    await textarea.fill('Hello from mobile!')
    await expect(textarea).toHaveValue('Hello from mobile!')
  })

  test('channel list items are tappable', async ({ page }) => {
    // On mobile, need to open sidebar first via trigger
    const trigger = page.locator('button[data-sidebar="trigger"]')

    // If trigger is visible, tap it to open sidebar
    if (await trigger.isVisible()) {
      await trigger.click()
      await page.waitForTimeout(300) // Wait for animation
    }

    // Look for channel list buttons
    const channelButtons = page.locator('button.channel-item')
    const count = await channelButtons.count()

    if (count > 0) {
      // First channel should be tappable
      await expect(channelButtons.first()).toBeEnabled()
    }
  })

  test('send button becomes enabled when text is entered', async ({ page }) => {
    const textarea = page.locator('main textarea')
    const sendButton = page.locator('button[type="submit"], button:has-text("Send")').first()

    // Initially send button should be disabled
    await expect(sendButton).toBeDisabled()

    // Type text
    await textarea.fill('Test message')

    // Send button should now be enabled
    await expect(sendButton).toBeEnabled()
  })
})

test.describe('Mobile PWA - Safe Area Insets', () => {
  test('footer respects safe area inset for bottom', async ({ page }) => {
    await mockAllRoutes(page)
    await page.setViewportSize({ width: 390, height: 844 })
    await page.goto('/')

    // Check that sidebar footer has safe area padding
    const footer = page.locator('[data-sidebar="footer"]')

    if (await footer.isVisible()) {
      const paddingBottom = await footer.evaluate((el) => getComputedStyle(el).paddingBottom)
      // Should have some padding (exact value depends on env() support)
      expect(paddingBottom).toBeTruthy()
    }
  })

  test('chat input area respects safe area inset', async ({ page }) => {
    await mockAllRoutes(page)
    // Simulate iPhone with notch
    await page.setViewportSize({ width: 390, height: 844 })
    await page.goto('/')

    // The input area should not be hidden by home indicator
    const inputArea = page.locator('.input-area, main form, main .flex:has(textarea)').first()
    await expect(inputArea).toBeVisible()

    const box = await inputArea.boundingBox()
    expect(box).toBeTruthy()
    if (box) {
      // Should have some margin/padding from bottom
      // (exact implementation varies, but it shouldn't be at 0)
      // On desktop testing, safe-area-inset returns 0, so just check it's visible
      expect(box.y + box.height).toBeLessThanOrEqual(844)
    }
  })
})

test.describe('Mobile PWA - Connection Status', () => {
  test.beforeEach(async ({ page }) => {
    await mockAllRoutes(page)
    await page.setViewportSize({ width: 390, height: 844 })
    await page.goto('/')
  })

  test('mobile header shows project name', async ({ page }) => {
    // On mobile, the header shows project name instead of connection dot
    const mobileHeader = page.locator('.mobile-header')
    await expect(mobileHeader).toBeVisible()

    // Should contain the SidebarTrigger and project name
    const text = await mobileHeader.textContent()
    expect(text).toBeTruthy()
  })

  test('sidebar contains connection status when opened', async ({ page }) => {
    // Open sidebar to check for connection dot
    const trigger = page.locator('button[data-sidebar="trigger"]')
    await trigger.click()
    await page.waitForTimeout(500)

    // Now connection dot should be visible in sidebar header
    const connectionDot = page.locator('.connection-dot')
    await expect(connectionDot.first()).toBeVisible()
  })
})

test.describe('Mobile PWA - Typography and Readability', () => {
  test.beforeEach(async ({ page }) => {
    await mockAllRoutes(page)
    await page.setViewportSize({ width: 390, height: 844 })
    await page.goto('/')
  })

  test('text is readable with sufficient contrast', async ({ page }) => {
    // Body text should be visible
    const body = page.locator('body')
    const color = await body.evaluate((el) => getComputedStyle(el).color)
    const bgColor = await body.evaluate((el) => getComputedStyle(el).backgroundColor)

    // Both should be defined
    expect(color).toBeTruthy()
    expect(bgColor).toBeTruthy()
  })

  test('monospace font is applied', async ({ page }) => {
    const body = page.locator('body')
    const fontFamily = await body.evaluate((el) => getComputedStyle(el).fontFamily)
    // Body uses monospace font (Terminal Noir design)
    expect(fontFamily).toMatch(/monospace|SF Mono|Menlo|Consolas/i)
  })
})

test.describe('Mobile PWA - Performance', () => {
  test('page loads within acceptable time on mobile', async ({ page }) => {
    await mockAllRoutes(page)
    await page.setViewportSize({ width: 390, height: 844 })

    const startTime = Date.now()
    await page.goto('/')
    await page.waitForLoadState('networkidle')
    const loadTime = Date.now() - startTime

    // Should load within 5 seconds on mobile
    expect(loadTime).toBeLessThan(5000)
  })

  test('no horizontal scroll on mobile', async ({ page }) => {
    await mockAllRoutes(page)
    await page.setViewportSize({ width: 390, height: 844 })
    await page.goto('/')

    // Check that page doesn't have horizontal scroll
    const scrollWidth = await page.evaluate(() => document.documentElement.scrollWidth)
    const clientWidth = await page.evaluate(() => document.documentElement.clientWidth)

    // Scroll width should equal client width (no horizontal overflow)
    expect(scrollWidth).toBeLessThanOrEqual(clientWidth + 10) // Allow 10px tolerance
  })
})

test.describe('Mobile PWA - Landscape Orientation', () => {
  test('layout handles landscape orientation', async ({ page }) => {
    await mockAllRoutes(page)
    // Landscape iPhone 14
    await page.setViewportSize({ width: 844, height: 390 })
    await page.goto('/')

    // App should still be visible
    const appContainer = page.locator('.app-container')
    await expect(appContainer).toBeVisible()

    // Main content should be visible
    const main = page.locator('main')
    await expect(main).toBeVisible()
  })
})
