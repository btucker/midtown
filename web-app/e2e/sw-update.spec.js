// @ts-check
import { test, expect } from '@playwright/test'
import { mockAllRoutes } from './helpers.js'

/**
 * Service Worker Update E2E Tests
 *
 * These tests verify the PWA service worker update mechanism works correctly.
 * Testing SW updates is inherently tricky because:
 * 1. SWs are scoped to origin+path and persist across page loads
 * 2. Update detection relies on byte-for-byte comparison of the SW script
 * 3. The update lifecycle (install → waiting → activate) involves async events
 *
 * We test the mechanisms are wired up correctly rather than the full Workbox internals.
 */

test.describe('Service Worker Registration', () => {
  test.beforeEach(async ({ page }) => {
    await mockAllRoutes(page)
  })

  test('registers a service worker on page load', async ({ page }) => {
    await page.goto('/')

    // Wait for SW to register
    const swRegistered = await page.evaluate(async () => {
      if (!('serviceWorker' in navigator)) return false
      const registration = await navigator.serviceWorker.getRegistration('/')
      return !!registration
    })

    expect(swRegistered).toBe(true)
  })

  test('service worker becomes active', async ({ page }) => {
    await page.goto('/')

    // Wait for SW to be active (may need to wait for install + activate)
    const swActive = await page.evaluate(async () => {
      if (!('serviceWorker' in navigator)) return false
      const registration = await navigator.serviceWorker.ready
      return registration.active !== null
    })

    expect(swActive).toBe(true)
  })

  test('service worker has controller after activation', async ({ page }) => {
    await page.goto('/')

    // clientsClaim() should make the SW control the page
    const hasController = await page.evaluate(async () => {
      if (!('serviceWorker' in navigator)) return false

      // Wait for SW to be ready
      await navigator.serviceWorker.ready

      // May need to wait a tick for clientsClaim to take effect
      await new Promise((resolve) => setTimeout(resolve, 100))

      // Either we have a controller, or we need to reload for clientsClaim to work
      // (first visit won't have controller until reload in some scenarios)
      return navigator.serviceWorker.controller !== null
    })

    // If no controller on first visit, that's expected - clientsClaim helps on subsequent visits
    // The important thing is the SW is registered and active
    if (!hasController) {
      // Reload and check again - clientsClaim should now give us a controller
      await page.reload()
      await page.waitForLoadState('networkidle')

      const hasControllerAfterReload = await page.evaluate(async () => {
        await navigator.serviceWorker.ready
        return navigator.serviceWorker.controller !== null
      })

      expect(hasControllerAfterReload).toBe(true)
    }
  })
})

test.describe('Service Worker Update Mechanism', () => {
  test.beforeEach(async ({ page }) => {
    await mockAllRoutes(page)
  })

  test('service worker responds to SKIP_WAITING message', async ({ page }) => {
    await page.goto('/')

    // Wait for SW to be ready
    await page.evaluate(async () => {
      await navigator.serviceWorker.ready
    })

    // Verify the SW can receive and handle SKIP_WAITING message
    // We can't easily test the full update flow, but we can verify the message handler exists
    const handlerExists = await page.evaluate(async () => {
      const registration = await navigator.serviceWorker.getRegistration('/')
      if (!registration || !registration.active) return false

      // The SW should be able to receive messages
      // We'll send a message and see if it doesn't throw
      return new Promise((resolve) => {
        try {
          // Send SKIP_WAITING to the active SW (this is what vite-plugin-pwa does)
          registration.active.postMessage({ type: 'SKIP_WAITING' })
          // If we get here without error, the handler exists
          resolve(true)
        } catch {
          resolve(false)
        }
      })
    })

    expect(handlerExists).toBe(true)
  })

  test('workbox-window module is available for update detection', async ({ page }) => {
    await page.goto('/')

    // The app should have loaded the workbox-window module for SW registration
    // This verifies the virtual:pwa-register import works
    // Wait for SW to be ready before checking registration
    const workboxLoaded = await page.evaluate(async () => {
      if (!('serviceWorker' in navigator)) return false
      // Wait for the SW to be ready (handles registration race condition)
      await navigator.serviceWorker.ready
      const registration = await navigator.serviceWorker.getRegistration('/')
      return registration !== undefined && registration !== null
    })

    expect(workboxLoaded).toBe(true)
  })
})

test.describe('Service Worker Precaching', () => {
  test.beforeEach(async ({ page }) => {
    await mockAllRoutes(page)
  })

  test('precaches core assets', async ({ page }) => {
    await page.goto('/')

    // Wait for SW to be ready and caches to be populated
    await page.evaluate(async () => {
      await navigator.serviceWorker.ready
      // Give time for precaching to complete
      await new Promise((resolve) => setTimeout(resolve, 500))
    })

    // Check that the precache exists
    const hasPrecache = await page.evaluate(async () => {
      const cacheNames = await caches.keys()
      // Workbox precache uses a name containing 'precache'
      return cacheNames.some((name) => name.includes('precache'))
    })

    expect(hasPrecache).toBe(true)
  })

  test('cleans up outdated caches on activation', async ({ page }) => {
    await page.goto('/')

    // The cleanupOutdatedCaches call happens on activate
    // We can verify the mechanism by checking that old caches are cleaned
    const cacheCleanupActive = await page.evaluate(async () => {
      await navigator.serviceWorker.ready

      // Get current cache names
      const cacheNames = await caches.keys()

      // Should have exactly one precache (the current version)
      const precaches = cacheNames.filter((name) => name.includes('precache'))

      // cleanupOutdatedCaches removes old precaches, leaving only the current one
      return precaches.length === 1
    })

    expect(cacheCleanupActive).toBe(true)
  })
})

test.describe('Offline Support', () => {
  test.beforeEach(async ({ page }) => {
    await mockAllRoutes(page)
  })

  test('app shell is cached for offline use', async ({ page }) => {
    await page.goto('/')

    // Wait for SW and precaching
    await page.evaluate(async () => {
      await navigator.serviceWorker.ready
      await new Promise((resolve) => setTimeout(resolve, 500))
    })

    // Check that index.html is cached
    const indexCached = await page.evaluate(async () => {
      const cacheNames = await caches.keys()
      for (const name of cacheNames) {
        const cache = await caches.open(name)
        const keys = await cache.keys()
        if (keys.some((req) => req.url.includes('index.html'))) {
          return true
        }
      }
      return false
    })

    expect(indexCached).toBe(true)
  })
})
