import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { get } from 'svelte/store'
import { usageData } from './store.js'
import { fetchUsage } from './api.js'
import { estimateTimeToFull, formatDurationEstimate } from './usage-utils.js'

describe('fetchUsage', () => {
  let originalFetch

  beforeEach(() => {
    originalFetch = globalThis.fetch
    // Reset store to empty array before each test
    usageData.set([])
  })

  afterEach(() => {
    globalThis.fetch = originalFetch
  })

  it('populates usageData on successful fetch', async () => {
    const mockResponse = {
      usage: [
        {
          provider: 'claude',
          profile: 'default',
          session_util: 42.5,
          session_resets: '2026-02-08T22:00:00Z',
          week_util: 15.3,
          week_resets: '2026-02-15T00:00:00Z',
          account_email: 'test@example.com',
        }
      ],
      // Backwards compatibility flat fields (not used by new frontend)
      session_util: 42.5,
      session_resets: '2026-02-08T22:00:00Z',
      week_util: 15.3,
      week_resets: '2026-02-15T00:00:00Z',
      account_email: 'test@example.com',
    }

    globalThis.fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: () => Promise.resolve(mockResponse),
    })

    await fetchUsage()
    expect(get(usageData)).toEqual(mockResponse.usage)
  })

  it('retains last-known data on network error', async () => {
    const previousData = [
      {
        provider: 'claude',
        profile: 'default',
        session_util: 30.0,
        session_resets: '2026-02-08T20:00:00Z',
        week_util: 10.0,
        week_resets: '2026-02-14T00:00:00Z',
        account_email: 'test@example.com',
      }
    ]
    usageData.set(previousData)

    globalThis.fetch = vi.fn().mockRejectedValue(new Error('Network error'))

    await fetchUsage()
    // Store should retain previous data, not be cleared to empty array
    expect(get(usageData)).toEqual(previousData)
  })

  it('retains last-known data on non-ok response', async () => {
    const previousData = [
      {
        provider: 'claude',
        profile: 'default',
        session_util: 50.0,
        session_resets: '2026-02-08T21:00:00Z',
        week_util: 20.0,
        week_resets: '2026-02-14T00:00:00Z',
        account_email: 'test@example.com',
      }
    ]
    usageData.set(previousData)

    globalThis.fetch = vi.fn().mockResolvedValue({
      ok: false,
      status: 500,
    })

    await fetchUsage()
    // Store should retain previous data on server error
    expect(get(usageData)).toEqual(previousData)
  })

  it('clears store on 204 No Content (no credentials)', async () => {
    // Real Fetch API: 204 is a 2xx status, so ok is true and body is empty.
    // fetchUsage must check for 204 before calling res.json().
    globalThis.fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 204,
      json: () => { throw new SyntaxError('Unexpected end of JSON input') },
    })

    await fetchUsage()
    expect(get(usageData)).toEqual([])
  })

  it('clears cached data on 204 No Content when store has previous data', async () => {
    const previousData = [
      {
        provider: 'claude',
        profile: 'default',
        session_util: 50.0,
        session_resets: '2026-02-08T21:00:00Z',
        week_util: 20.0,
        week_resets: '2026-02-14T00:00:00Z',
        account_email: 'test@example.com',
      }
    ]
    usageData.set(previousData)

    // 204 means credentials are gone — must clear the store, not retain stale data
    globalThis.fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 204,
      json: () => { throw new SyntaxError('Unexpected end of JSON input') },
    })

    await fetchUsage()
    expect(get(usageData)).toEqual([])
  })

  it('handles multiple accounts in usage array', async () => {
    const mockResponse = {
      usage: [
        {
          provider: 'claude',
          profile: 'default',
          session_util: 42.5,
          session_resets: '2026-02-08T22:00:00Z',
          week_util: 15.3,
          week_resets: '2026-02-15T00:00:00Z',
          account_email: 'user1@example.com',
        },
        {
          provider: 'claude',
          profile: 'work',
          session_util: 60.0,
          session_resets: '2026-02-08T23:00:00Z',
          week_util: 30.0,
          week_resets: '2026-02-15T00:00:00Z',
          account_email: 'user2@example.com',
        }
      ],
      // Backwards compatibility flat fields for primary account
      session_util: 42.5,
      session_resets: '2026-02-08T22:00:00Z',
      week_util: 15.3,
      week_resets: '2026-02-15T00:00:00Z',
      account_email: 'user1@example.com',
    }

    globalThis.fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: () => Promise.resolve(mockResponse),
    })

    await fetchUsage()
    expect(get(usageData)).toEqual(mockResponse.usage)
    expect(get(usageData)).toHaveLength(2)
  })
})

describe('estimateTimeToFull', () => {
  it('returns null for invalid date strings (NaN guard)', () => {
    // Invalid date causes new Date().getTime() to return NaN.
    // NaN <= 0 is false in JavaScript, so without an explicit guard
    // NaN propagates through the calculation.
    const result = estimateTimeToFull(50, 'not-a-date', true)
    expect(result).toBeNull()
  })

  it('returns null when utilization is 0 or 100', () => {
    const futureDate = new Date(Date.now() + 3600 * 1000).toISOString()
    expect(estimateTimeToFull(0, futureDate, true)).toBeNull()
    expect(estimateTimeToFull(100, futureDate, true)).toBeNull()
  })

  it('returns null when reset time is in the past', () => {
    const pastDate = new Date(Date.now() - 3600 * 1000).toISOString()
    expect(estimateTimeToFull(50, pastDate, true)).toBeNull()
  })

  it('returns a duration string for valid inputs', () => {
    // Set reset time to 2 hours from now (session window is 5h)
    const resetDate = new Date(Date.now() + 2 * 3600 * 1000).toISOString()
    const result = estimateTimeToFull(50, resetDate, true)
    expect(result).not.toBeNull()
    expect(result).toMatch(/^~\d+[hm]/)
  })
})

describe('formatDurationEstimate', () => {
  it('formats seconds less than 1 minute', () => {
    expect(formatDurationEstimate(10)).toBe('~<1m left')
  })

  it('formats minutes', () => {
    expect(formatDurationEstimate(300)).toBe('~5m left')
  })

  it('formats exact hours', () => {
    expect(formatDurationEstimate(7200)).toBe('~2h left')
  })

  it('formats hours and minutes', () => {
    expect(formatDurationEstimate(5400)).toBe('~1h30m left')
  })
})
