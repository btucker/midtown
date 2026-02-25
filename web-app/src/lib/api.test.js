import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { get } from 'svelte/store'
import { messagesByChannel } from './store.js'
import { fetchHistory } from './api.js'

describe('fetchHistory', () => {
  let originalFetch

  beforeEach(() => {
    originalFetch = globalThis.fetch
    // Reset store to known state: channel-a has existing messages
    messagesByChannel.set({
      midtown: [],
      'channel-a': [{ id: 1, content: 'existing message', channel: 'channel-a' }],
    })
  })

  afterEach(() => {
    globalThis.fetch = originalFetch
  })

  it('preserves existing channel messages when bulk fetch only returns other channels', async () => {
    // Regression: fetchHistory() (no param) called on WS reconnect was doing
    // messagesByChannel.set(byChannel) which wiped channels not in the response.
    // If the server only returns messages for channel-b, channel-a should NOT be cleared.
    globalThis.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => [
        { id: 2, content: 'new message', channel: 'channel-b', timestamp: '2026-01-01T00:00:00Z' },
      ],
    })

    await fetchHistory()

    const store = get(messagesByChannel)
    // channel-b should have the new message
    expect(store['channel-b']).toHaveLength(1)
    // channel-a must NOT have been wiped
    expect(store['channel-a']).toHaveLength(1)
    expect(store['channel-a'][0].content).toBe('existing message')
  })

  it('updates existing channel data when bulk fetch returns fresh messages for that channel', async () => {
    globalThis.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => [
        { id: 10, content: 'fresh message', channel: 'channel-a', timestamp: '2026-01-01T00:00:00Z' },
      ],
    })

    await fetchHistory()

    const store = get(messagesByChannel)
    // channel-a should have the fresh data (overriding the old single message)
    expect(store['channel-a']).toHaveLength(1)
    expect(store['channel-a'][0].content).toBe('fresh message')
  })
})
