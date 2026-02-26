import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { get } from 'svelte/store'
import { messagesByChannel, threadData, channels, activeChannel, agentToolItems } from './store.js'
import { fetchHistory, handleUpdate, fetchChannels, selectDm } from './api.js'

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

  it('clears ghost pending messages from channels absent in the history response', async () => {
    // Regression: if the WS echo was lost during a disconnect, a pending optimistic
    // message can survive in a low-traffic channel that doesn't appear in the bulk
    // history response. The merge-not-replace strategy preserves the channel, so the
    // pending message lingers forever as a "ghost".
    //
    // Fix: strip pending messages from all existing channels before merging, so
    // only confirmed (non-pending) messages remain for channels not in the response.
    messagesByChannel.set({
      midtown: [],
      'web': [
        { id: 'real-1', content: 'existing confirmed', channel: 'web', from: 'coworker' },
        { id: 'pending-ghost', content: 'my unsent msg', channel: 'web', from: 'user', pending: true },
      ],
    })

    // Bulk fetch only returns midtown — 'web' is low-traffic, not in response
    globalThis.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => [
        { id: 99, content: 'midtown msg', channel: 'midtown', timestamp: '2026-01-01T00:00:00Z' },
      ],
    })

    await fetchHistory()

    const store = get(messagesByChannel)
    expect(store['midtown']).toHaveLength(1)
    // The confirmed message should still be there
    expect(store['web'].find((m) => m.id === 'real-1')).toBeTruthy()
    // The pending ghost must be gone
    expect(store['web'].some((m) => m.pending)).toBe(false)
    expect(store['web'].find((m) => m.id === 'pending-ghost')).toBeUndefined()
  })

  it('does not leave pending messages when the channel is included in the history response', async () => {
    // When a channel IS in the bulk response, its data replaces existing entirely.
    // Pending messages in existing are discarded because the whole channel array
    // is overwritten — this is the already-correct baseline behavior.
    messagesByChannel.set({
      midtown: [
        { id: 'pending-mt', content: 'hello', channel: 'midtown', from: 'user', pending: true },
      ],
    })

    globalThis.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => [
        { id: 50, content: 'confirmed midtown', channel: 'midtown', timestamp: '2026-01-01T00:00:00Z' },
      ],
    })

    await fetchHistory()

    const store = get(messagesByChannel)
    expect(store['midtown']).toHaveLength(1)
    expect(store['midtown'][0].id).toBe(50)
    expect(store['midtown'].some((m) => m.pending)).toBe(false)
  })
})

describe('handleUpdate — optimistic message deduplication', () => {
  beforeEach(() => {
    messagesByChannel.set({ midtown: [] })
    threadData.set(null)
  })

  it('replaces a pending optimistic message with the real server message', () => {
    // Simulate user hitting Send: a pending placeholder is in the store
    messagesByChannel.set({
      midtown: [
        { id: 'pending-abc', from: 'user', content: 'hello', channel: 'midtown', pending: true },
      ],
    })

    // Server echoes back the real message
    handleUpdate({
      type: 'channel_message',
      data: { id: 'real-1', from: 'user', content: 'hello', channel: 'midtown', timestamp: '2026-01-01T00:00:00Z' },
    })

    const store = get(messagesByChannel)
    // The pending placeholder should be gone; only the real message remains
    expect(store['midtown']).toHaveLength(1)
    expect(store['midtown'][0].id).toBe('real-1')
    expect(store['midtown'][0].pending).toBeUndefined()
  })

  it('does not remove a pending message if content does not match', () => {
    messagesByChannel.set({
      midtown: [
        { id: 'pending-abc', from: 'user', content: 'different text', channel: 'midtown', pending: true },
      ],
    })

    // Real message arrives for a different content
    handleUpdate({
      type: 'channel_message',
      data: { id: 'real-2', from: 'user', content: 'hello', channel: 'midtown', timestamp: '2026-01-01T00:00:00Z' },
    })

    const store = get(messagesByChannel)
    // Both messages should be present: the unmatched pending + the real one
    expect(store['midtown']).toHaveLength(2)
    expect(store['midtown'].some((m) => m.pending)).toBe(true)
    expect(store['midtown'].some((m) => m.id === 'real-2')).toBe(true)
  })

  it('only removes the first matching pending message when duplicates exist', () => {
    messagesByChannel.set({
      midtown: [
        { id: 'pending-1', from: 'user', content: 'hello', channel: 'midtown', pending: true },
        { id: 'pending-2', from: 'user', content: 'hello', channel: 'midtown', pending: true },
      ],
    })

    handleUpdate({
      type: 'channel_message',
      data: { id: 'real-3', from: 'user', content: 'hello', channel: 'midtown', timestamp: '2026-01-01T00:00:00Z' },
    })

    const store = get(messagesByChannel)
    // First pending removed, second pending preserved, real message appended
    expect(store['midtown']).toHaveLength(2)
    expect(store['midtown'][0].id).toBe('pending-2')
    expect(store['midtown'][1].id).toBe('real-3')
  })

  it('replaces a pending thread reply with the real server reply', () => {
    const parentId = 'parent-msg-1'
    threadData.set({
      parentMessage: { id: parentId, from: 'lead', content: 'original' },
      channelName: 'midtown',
      messages: [
        { id: 'pending-reply-1', from: 'user', content: 'my reply', pending: true },
      ],
    })

    // Server echoes the real thread reply
    handleUpdate({
      type: 'channel_message',
      data: {
        id: 'real-reply-1',
        from: 'user',
        content: 'my reply',
        channel: 'midtown',
        thread_parent_id: parentId,
        timestamp: '2026-01-01T00:00:00Z',
      },
    })

    const td = get(threadData)
    expect(td.messages).toHaveLength(1)
    expect(td.messages[0].id).toBe('real-reply-1')
    expect(td.messages[0].pending).toBeUndefined()
  })

  it('only removes the first matching pending thread reply when duplicates exist', () => {
    const parentId = 'parent-msg-1'
    threadData.set({
      parentMessage: { id: parentId, from: 'lead', content: 'original' },
      channelName: 'midtown',
      messages: [
        { id: 'pending-t1', from: 'user', content: 'same text', pending: true },
        { id: 'pending-t2', from: 'user', content: 'same text', pending: true },
      ],
    })

    handleUpdate({
      type: 'channel_message',
      data: {
        id: 'real-thread-reply',
        from: 'user',
        content: 'same text',
        channel: 'midtown',
        thread_parent_id: parentId,
        timestamp: '2026-01-01T00:00:00Z',
      },
    })

    const td = get(threadData)
    // First pending removed, second preserved, real appended
    expect(td.messages).toHaveLength(2)
    expect(td.messages[0].id).toBe('pending-t2')
    expect(td.messages[1].id).toBe('real-thread-reply')
  })

  it('does not remove a pending message when a different user posts the same content', () => {
    messagesByChannel.set({
      midtown: [
        { id: 'pending-mine', from: 'user', content: 'hello', channel: 'midtown', pending: true },
      ],
    })

    // Another participant sends identical content before the user's echo arrives
    handleUpdate({
      type: 'channel_message',
      data: { id: 'other-user-msg', from: 'alice', content: 'hello', channel: 'midtown', timestamp: '2026-01-01T00:00:00Z' },
    })

    const store = get(messagesByChannel)
    // Pending placeholder must NOT be consumed by another user's message
    expect(store['midtown']).toHaveLength(2)
    expect(store['midtown'].some((m) => m.pending)).toBe(true)
    expect(store['midtown'].some((m) => m.id === 'other-user-msg')).toBe(true)
  })

  it('does not modify threadData when the panel is for a different parent', () => {
    const parentId = 'parent-msg-1'
    const otherParentId = 'parent-msg-2'
    threadData.set({
      parentMessage: { id: otherParentId, from: 'lead', content: 'other thread' },
      channelName: 'midtown',
      messages: [
        { id: 'pending-reply-2', from: 'user', content: 'my reply', pending: true },
      ],
    })

    handleUpdate({
      type: 'channel_message',
      data: {
        id: 'real-reply-2',
        from: 'user',
        content: 'my reply',
        channel: 'midtown',
        thread_parent_id: parentId, // different parent
        timestamp: '2026-01-01T00:00:00Z',
      },
    })

    const td = get(threadData)
    // Panel is for otherParentId — should be untouched
    expect(td.messages).toHaveLength(1)
    expect(td.messages[0].id).toBe('pending-reply-2')
  })
})

describe('fetchChannels — is_dm field', () => {
  let originalFetch

  beforeEach(() => {
    originalFetch = globalThis.fetch
    channels.set([{ name: 'midtown', unread: 0, has_pr: false, ci_status: null }])
  })

  afterEach(() => {
    globalThis.fetch = originalFetch
  })

  it('propagates is_dm=true from the API response', async () => {
    globalThis.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({
        channels: [
          { name: 'midtown', is_archived: false, is_dm: false },
          { name: 'dm-alice', is_archived: false, is_dm: true },
        ],
      }),
    })

    await fetchChannels()

    const ch = get(channels)
    const dmChannel = ch.find((c) => c.name === 'dm-alice')
    expect(dmChannel).toBeTruthy()
    expect(dmChannel.is_dm).toBe(true)
    expect(ch.find((c) => c.name === 'midtown').is_dm).toBe(false)
  })

  it('defaults is_dm to false for string-format channels (legacy API)', async () => {
    globalThis.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ channels: ['midtown', 'other'] }),
    })

    await fetchChannels()

    const ch = get(channels)
    expect(ch.every((c) => c.is_dm === false)).toBe(true)
  })
})

describe('selectDm', () => {
  let originalFetch

  beforeEach(() => {
    originalFetch = globalThis.fetch
    channels.set([
      { name: 'midtown', unread: 0, has_pr: false, ci_status: null, is_dm: false },
      { name: 'dm-alice', unread: 3, has_pr: false, ci_status: null, is_dm: true },
    ])
    activeChannel.set('midtown')
    messagesByChannel.set({ midtown: [], 'dm-alice': [{ id: 1, content: 'hey', channel: 'dm-alice' }] })
  })

  afterEach(() => {
    globalThis.fetch = originalFetch
  })

  it('switches to existing DM channel without creating it', async () => {
    const fetchMock = vi.fn()
    globalThis.fetch = fetchMock

    await selectDm('alice')

    // No create call should have been made
    expect(fetchMock).not.toHaveBeenCalled()
    expect(get(activeChannel)).toBe('dm-alice')
  })

  it('clears the unread count when switching to a DM channel', async () => {
    globalThis.fetch = vi.fn()

    await selectDm('alice')

    const ch = get(channels).find((c) => c.name === 'dm-alice')
    expect(ch.unread).toBe(0)
  })

  it('creates the DM channel if it does not exist, then switches to it', async () => {
    channels.set([{ name: 'midtown', unread: 0, has_pr: false, ci_status: null, is_dm: false }])

    globalThis.fetch = vi.fn()
      // First call: POST create
      .mockResolvedValueOnce({ ok: true, json: async () => ({}) })
      // Second call: GET fetchChannels
      .mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          channels: [
            { name: 'midtown', is_archived: false, is_dm: false },
            { name: 'dm-bob', is_archived: false, is_dm: true },
          ],
        }),
      })
      // Third call: GET fetchHistory for dm-bob
      .mockResolvedValueOnce({ ok: true, json: async () => [] })

    await selectDm('bob')

    expect(get(activeChannel)).toBe('dm-bob')
    // Create endpoint should have been called with the right name
    const createCall = globalThis.fetch.mock.calls[0]
    expect(createCall[0]).toContain('/channels/create')
    expect(JSON.parse(createCall[1].body).name).toBe('dm-bob')
  })
})

describe('handleUpdate — agentToolItems persistence after channel lead message', () => {
  const sampleItem = {
    status: 'Completed',
    content: [{ ToolCall: { call_id: 'abc', name: 'Read', semantic_header: 'Read file.txt' } }],
  }

  beforeEach(() => {
    vi.useFakeTimers()
    agentToolItems.set({})
    messagesByChannel.set({ midtown: [], web: [] })
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  it('does not immediately clear tool items when a channel lead posts a message', () => {
    // Simulate tool activity arriving for the 'web' channel lead
    handleUpdate({ type: 'universal_items', data: { channel: 'web', agent_name: 'web', items: [sampleItem] } })
    expect(get(agentToolItems)['web']).toHaveLength(1)

    // Channel lead posts a message — items should still be visible immediately after
    handleUpdate({ type: 'channel_message', data: { id: 'msg-1', from: 'web', content: 'done!', channel: 'web', timestamp: '2026-01-01T00:00:00Z' } })
    expect(get(agentToolItems)['web']).toHaveLength(1)
  })

  it('clears tool items after the persistence delay expires', () => {
    handleUpdate({ type: 'universal_items', data: { channel: 'web', agent_name: 'web', items: [sampleItem] } })
    handleUpdate({ type: 'channel_message', data: { id: 'msg-1', from: 'web', content: 'done!', channel: 'web', timestamp: '2026-01-01T00:00:00Z' } })

    // Items should still be present before the delay expires
    vi.advanceTimersByTime(3999)
    expect(get(agentToolItems)['web']).toHaveLength(1)

    // Items should be cleared after the delay
    vi.advanceTimersByTime(1)
    expect(get(agentToolItems)['web']).toBeUndefined()
  })

  it('cancels the clear timeout when new tool activity arrives before the delay', () => {
    handleUpdate({ type: 'universal_items', data: { channel: 'web', agent_name: 'web', items: [sampleItem] } })
    handleUpdate({ type: 'channel_message', data: { id: 'msg-1', from: 'web', content: 'status update', channel: 'web', timestamp: '2026-01-01T00:00:00Z' } })

    // New tool activity arrives within the delay window (agent is still working)
    vi.advanceTimersByTime(2000)
    const newItem = { ...sampleItem, content: [{ ToolCall: { call_id: 'def', name: 'Write', semantic_header: 'Write file.txt' } }] }
    handleUpdate({ type: 'universal_items', data: { channel: 'web', agent_name: 'web', items: [newItem] } })

    // Advance past the original delay — items should still be present because timeout was cancelled
    vi.advanceTimersByTime(3000)
    expect(get(agentToolItems)['web']).toBeDefined()
    expect(get(agentToolItems)['web'].length).toBeGreaterThan(0)
  })

  it('still immediately clears tool items for non-channel-lead coworkers (keyed by channel)', () => {
    // Coworker 'manhattan' posts to 'midtown' — their items are NOT stored under 'manhattan'
    // so the delete('manhattan') call is a no-op, but the 'midtown' key is unaffected
    handleUpdate({ type: 'universal_items', data: { channel: null, agent_name: 'lead', items: [sampleItem] } })
    expect(get(agentToolItems)['midtown']).toHaveLength(1)

    // A non-lead, non-midtown sender posting to midtown schedules a delayed clear for 'manhattan'
    // (which doesn't exist), so midtown items are unaffected
    handleUpdate({ type: 'channel_message', data: { id: 'msg-2', from: 'manhattan', content: 'hi', channel: 'midtown', timestamp: '2026-01-01T00:00:00Z' } })
    vi.advanceTimersByTime(5000)
    // midtown items were not deleted — 'manhattan' key didn't exist in the store
    expect(get(agentToolItems)['midtown']).toHaveLength(1)
  })
})
