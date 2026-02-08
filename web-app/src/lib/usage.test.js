import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { get } from 'svelte/store'
import { usageData } from './store.js'
import { fetchUsage } from './api.js'

describe('fetchUsage', () => {
  let originalFetch

  beforeEach(() => {
    originalFetch = globalThis.fetch
    // Reset store to null before each test
    usageData.set(null)
  })

  afterEach(() => {
    globalThis.fetch = originalFetch
  })

  it('populates usageData on successful fetch', async () => {
    const mockData = {
      session_util: 42.5,
      session_resets: '2026-02-08T22:00:00Z',
      week_util: 15.3,
      week_resets: '2026-02-15T00:00:00Z',
      account_email: 'test@example.com',
    }

    globalThis.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: () => Promise.resolve(mockData),
    })

    await fetchUsage()
    expect(get(usageData)).toEqual(mockData)
  })

  it('retains last-known data on network error', async () => {
    const previousData = {
      session_util: 30.0,
      session_resets: '2026-02-08T20:00:00Z',
      week_util: 10.0,
      week_resets: '2026-02-14T00:00:00Z',
      account_email: 'test@example.com',
    }
    usageData.set(previousData)

    globalThis.fetch = vi.fn().mockRejectedValue(new Error('Network error'))

    await fetchUsage()
    // Store should retain previous data, not be cleared to null
    expect(get(usageData)).toEqual(previousData)
  })

  it('retains last-known data on non-ok response', async () => {
    const previousData = {
      session_util: 50.0,
      session_resets: '2026-02-08T21:00:00Z',
      week_util: 20.0,
      week_resets: '2026-02-14T00:00:00Z',
      account_email: 'test@example.com',
    }
    usageData.set(previousData)

    globalThis.fetch = vi.fn().mockResolvedValue({
      ok: false,
      status: 500,
    })

    await fetchUsage()
    // Store should retain previous data on server error
    expect(get(usageData)).toEqual(previousData)
  })

  it('leaves store as null on 204 No Content (no credentials)', async () => {
    globalThis.fetch = vi.fn().mockResolvedValue({
      ok: false,
      status: 204,
    })

    await fetchUsage()
    // No previous data, 204 response — store stays null
    expect(get(usageData)).toBeNull()
  })
})
