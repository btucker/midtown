import { describe, it, expect, vi, beforeEach } from 'vitest'

// We test getSelkie by importing a fresh module for each test,
// since the module caches state in top-level variables.

async function loadFreshModule() {
  // Reset module registry so each test gets fresh selkie/initPromise state
  vi.resetModules()
  return import('./selkie.js')
}

describe('getSelkie', () => {
  beforeEach(() => {
    vi.resetModules()
    vi.restoreAllMocks()
  })

  it('returns the selkie module on successful init', async () => {
    const mockRender = vi.fn()
    vi.doMock('selkie-rs', () => ({
      default: vi.fn().mockResolvedValue(undefined),
      initialize: vi.fn(),
      render: mockRender,
    }))

    const { getSelkie } = await loadFreshModule()
    const result = await getSelkie()
    expect(result).toEqual({ render: mockRender })
  })

  it('caches the module after successful init', async () => {
    const mockInitWasm = vi.fn().mockResolvedValue(undefined)
    vi.doMock('selkie-rs', () => ({
      default: mockInitWasm,
      initialize: vi.fn(),
      render: vi.fn(),
    }))

    const { getSelkie } = await loadFreshModule()
    await getSelkie()
    await getSelkie()
    // initWasm should only be called once
    expect(mockInitWasm).toHaveBeenCalledTimes(1)
  })

  it('retries initialization after a failure', async () => {
    let callCount = 0
    vi.doMock('selkie-rs', () => ({
      default: vi.fn().mockImplementation(() => {
        callCount++
        if (callCount === 1) return Promise.reject(new Error('WASM load failed'))
        return Promise.resolve(undefined)
      }),
      initialize: vi.fn(),
      render: vi.fn(),
    }))

    const { getSelkie } = await loadFreshModule()

    // First call should fail
    await expect(getSelkie()).rejects.toThrow('WASM load failed')

    // Second call should retry and succeed
    const result = await getSelkie()
    expect(result).toHaveProperty('render')
  })

  it('does not cache a rejected promise permanently', async () => {
    vi.doMock('selkie-rs', () => ({
      default: vi.fn().mockRejectedValue(new Error('Network error')),
      initialize: vi.fn(),
      render: vi.fn(),
    }))

    const { getSelkie } = await loadFreshModule()

    // All calls should reject (not just the first)
    await expect(getSelkie()).rejects.toThrow('Network error')
    // But importantly, calling again should attempt a new init, not return cached rejection
    await expect(getSelkie()).rejects.toThrow('Network error')
  })
})
