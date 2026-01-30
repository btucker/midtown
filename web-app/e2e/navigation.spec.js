// @ts-check
import { test, expect } from '@playwright/test'
import { mockAllRoutes } from './helpers.js'

test.describe('Navigation', () => {
  test.beforeEach(async ({ page }) => {
    await mockAllRoutes(page)
    await page.goto('/')
  })

  test('header shows app title and connection status', async ({ page }) => {
    await expect(page.locator('h1')).toHaveText('Midtown')
    await expect(page.locator('.connection-status')).toBeVisible()
  })

  test('three nav tabs are rendered: Channel, Status, Lead', async ({ page }) => {
    const nav = page.locator('nav')
    const buttons = nav.getByRole('button')
    await expect(buttons).toHaveCount(3)
    await expect(buttons.nth(0)).toHaveText('Channel')
    await expect(buttons.nth(1)).toHaveText('Status')
    await expect(buttons.nth(2)).toHaveText('Tmux')
  })

  test('Channel tab is active by default', async ({ page }) => {
    const channelBtn = page.locator('nav').getByRole('button', { name: 'Channel' })
    await expect(channelBtn).toHaveClass(/active/)

    // Channel content visible
    await expect(page.locator('.channel-container')).toBeVisible()
    // Other content not visible
    await expect(page.locator('.status-container')).toHaveCount(0)
    await expect(page.locator('.tmux-container')).toHaveCount(0)
  })

  test('switches to Status tab', async ({ page }) => {
    const nav = page.locator('nav')
    await nav.getByRole('button', { name: 'Status' }).click()

    await expect(nav.getByRole('button', { name: 'Status' })).toHaveClass(/active/)
    await expect(nav.getByRole('button', { name: 'Channel' })).not.toHaveClass(/active/)

    await expect(page.locator('.status-container')).toBeVisible()
    await expect(page.locator('.channel-container')).toHaveCount(0)
  })

  test('switches to Lead tab', async ({ page }) => {
    const nav = page.locator('nav')
    await nav.getByRole('button', { name: 'Tmux' }).click()

    await expect(nav.getByRole('button', { name: 'Tmux' })).toHaveClass(/active/)
    await expect(page.locator('.tmux-container')).toBeVisible()
    await expect(page.locator('.channel-container')).toHaveCount(0)
  })

  test('switches back to Channel from Lead', async ({ page }) => {
    const nav = page.locator('nav')
    await nav.getByRole('button', { name: 'Tmux' }).click()
    await expect(page.locator('.tmux-container')).toBeVisible()

    await nav.getByRole('button', { name: 'Channel' }).click()
    await expect(page.locator('.channel-container')).toBeVisible()
    await expect(page.locator('.tmux-container')).toHaveCount(0)
  })

  test('kanban board is always visible below nav regardless of active tab', async ({ page }) => {
    const kanban = page.locator('.kanban >> nth=0')
    const nav = page.locator('nav')

    // Channel tab
    await expect(kanban).toBeVisible()

    // Status tab
    await nav.getByRole('button', { name: 'Status' }).click()
    await expect(kanban).toBeVisible()

    // Lead tab
    await nav.getByRole('button', { name: 'Tmux' }).click()
    await expect(kanban).toBeVisible()
  })

  test('active tab has accent color border', async ({ page }) => {
    const channelBtn = page.locator('nav').getByRole('button', { name: 'Channel' })
    // Active tab has bottom border color #5fafaf = rgb(95, 175, 175)
    await expect(channelBtn).toHaveCSS('border-bottom-color', 'rgb(95, 175, 175)')
  })
})
