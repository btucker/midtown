import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { get } from 'svelte/store'
import { messagesByChannel, threadData, channels, activeChannel, activeProject, agentToolItems, threadToolItems } from './store.js'
import { fetchHistory, handleUpdate, fetchChannels, selectDm, switchProject } from './api.js'

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

  it('still navigates to DM channel when backend creation returns an error', async () => {
    // Regression: selectDm used to call `return` after a non-ok response, leaving
    // activeChannel unchanged and giving the user no visible feedback.
    channels.set([{ name: 'midtown', unread: 0, has_pr: false, ci_status: null, is_dm: false }])

    globalThis.fetch = vi.fn().mockResolvedValueOnce({
      ok: false,
      json: async () => ({ error: 'internal server error' }),
    })

    await selectDm('carol')

    // Despite the backend failure, the user should land on the DM channel
    expect(get(activeChannel)).toBe('dm-carol')
    // The channel should be in the sidebar as a DM
    const ch = get(channels).find((c) => c.name === 'dm-carol')
    expect(ch).toBeTruthy()
    expect(ch.is_dm).toBe(true)
  })

  it('still navigates to DM channel when fetchChannels fails after creation', async () => {
    // Regression: selectDm used to call `return` inside the catch block when
    // fetchChannels threw, leaving activeChannel unchanged.
    channels.set([{ name: 'midtown', unread: 0, has_pr: false, ci_status: null, is_dm: false }])

    globalThis.fetch = vi.fn()
      .mockResolvedValueOnce({ ok: true, json: async () => ({}) }) // create succeeds
      .mockRejectedValueOnce(new Error('network error')) // fetchChannels fails

    await selectDm('dave')

    expect(get(activeChannel)).toBe('dm-dave')
    const ch = get(channels).find((c) => c.name === 'dm-dave')
    expect(ch).toBeTruthy()
    expect(ch.is_dm).toBe(true)
  })
})

describe('fetchChannels — is_dm name-prefix fallback', () => {
  let originalFetch

  beforeEach(() => {
    originalFetch = globalThis.fetch
    channels.set([{ name: 'midtown', unread: 0, has_pr: false, ci_status: null }])
  })

  afterEach(() => {
    globalThis.fetch = originalFetch
  })

  it('marks dm- prefixed channels as DMs when is_dm field is absent from API response', async () => {
    // Regression: if the backend omits is_dm (or sends undefined) for a dm-* channel,
    // fetchChannels stored is_dm=undefined (falsy), and ChannelList filtered it out
    // so the DM section never appeared.
    globalThis.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({
        channels: [
          { name: 'midtown', is_archived: false, is_dm: false },
          { name: 'dm-eve', is_archived: false }, // no is_dm field
        ],
      }),
    })

    await fetchChannels()

    const ch = get(channels).find((c) => c.name === 'dm-eve')
    expect(ch).toBeTruthy()
    expect(ch.is_dm).toBe(true)
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
    // Tests in this block use 'midtown' as the active project so that
    // universal_items with channel=null route to 'midtown' (not hardcoded).
    activeProject.set('midtown')
  })

  afterEach(() => {
    vi.useRealTimers()
    activeProject.set(null)
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

  it('clears midtown tool items after delay when a coworker posts to midtown', () => {
    // agentToolItems is channel-keyed: a coworker posting to 'midtown' schedules
    // a delayed clear for the 'midtown' key, not a no-op on their sender name.
    handleUpdate({ type: 'universal_items', data: { channel: null, agent_name: 'lead', items: [sampleItem] } })
    expect(get(agentToolItems)['midtown']).toHaveLength(1)

    handleUpdate({ type: 'channel_message', data: { id: 'msg-2', from: 'manhattan', content: 'hi', channel: 'midtown', timestamp: '2026-01-01T00:00:00Z' } })

    // Items still present before the delay
    vi.advanceTimersByTime(3999)
    expect(get(agentToolItems)['midtown']).toHaveLength(1)

    // Items cleared after the delay
    vi.advanceTimersByTime(1)
    expect(get(agentToolItems)['midtown']).toBeUndefined()
  })

  it('clears topic channel tool items when the main lead posts to that channel', () => {
    // Regression: the original guard blocked the clear when msg.from was 'lead' or
    // 'midtown', regardless of which channel the message was posted to. A lead
    // posting to 'web' should still schedule a clear for 'web' tool items.
    handleUpdate({ type: 'universal_items', data: { channel: 'web', agent_name: 'web', items: [sampleItem] } })
    expect(get(agentToolItems)['web']).toHaveLength(1)

    handleUpdate({ type: 'channel_message', data: { id: 'msg-3', from: 'lead', content: 'hi', channel: 'web', timestamp: '2026-01-01T00:00:00Z' } })

    // Items present before delay
    vi.advanceTimersByTime(3999)
    expect(get(agentToolItems)['web']).toHaveLength(1)

    // Items cleared after delay
    vi.advanceTimersByTime(1)
    expect(get(agentToolItems)['web']).toBeUndefined()
  })

  it('does not clear tool items for an unrelated channel when another channel gets a message', () => {
    // Channel isolation: clearing 'web' must not affect 'staging'.
    handleUpdate({ type: 'universal_items', data: { channel: 'web', agent_name: 'web', items: [sampleItem] } })
    handleUpdate({ type: 'universal_items', data: { channel: 'staging', agent_name: 'staging', items: [sampleItem] } })

    // Message arrives only on 'web'
    handleUpdate({ type: 'channel_message', data: { id: 'msg-4', from: 'web', content: 'done', channel: 'web', timestamp: '2026-01-01T00:00:00Z' } })

    // Advance past the clear delay
    vi.advanceTimersByTime(5000)

    // 'web' items cleared, 'staging' items untouched
    expect(get(agentToolItems)['web']).toBeUndefined()
    expect(get(agentToolItems)['staging']).toHaveLength(1)
  })

  it('cancels pending clear timeouts when switching projects', () => {
    // Regression: pending setTimeout handles in agentClearTimeouts were not
    // cancelled on switchProject(), so a delayed clear could fire against the
    // new project's agentToolItems store after the switch.
    handleUpdate({ type: 'universal_items', data: { channel: 'web', agent_name: 'web', items: [sampleItem] } })
    handleUpdate({ type: 'channel_message', data: { id: 'msg-5', from: 'web', content: 'done', channel: 'web', timestamp: '2026-01-01T00:00:00Z' } })

    // Switch projects while the 4-second clear is still pending
    vi.advanceTimersByTime(1000)
    switchProject('other-project', null)

    // Simulate new tool items arriving in the new project
    handleUpdate({ type: 'universal_items', data: { channel: 'web', agent_name: 'web', items: [sampleItem] } })

    // Advance past the original delay — the stale timeout must NOT fire
    vi.advanceTimersByTime(5000)

    // New project's 'web' tool items must not have been cleared by the old timeout
    expect(get(agentToolItems)['web']).toHaveLength(1)
  })
})

describe('switchProject — initial channel state uses project name, not hardcoded midtown', () => {
  beforeEach(() => {
    channels.set([])
    activeChannel.set(null)
    messagesByChannel.set({})
  })

  it('sets activeChannel to the project name', () => {
    switchProject('my-project', null)
    expect(get(activeChannel)).toBe('my-project')
  })

  it('initializes messagesByChannel keyed by project name', () => {
    switchProject('my-project', null)
    const store = get(messagesByChannel)
    expect(Object.keys(store)).toContain('my-project')
    expect(Object.keys(store)).not.toContain('midtown')
  })

  it('initializes channels list with the project name, not midtown', () => {
    switchProject('my-project', null)
    const ch = get(channels)
    expect(ch.some((c) => c.name === 'my-project')).toBe(true)
    expect(ch.some((c) => c.name === 'midtown')).toBe(false)
  })
})

describe('fetchHistory — channelless messages use activeProject, not hardcoded midtown', () => {
  let originalFetch

  beforeEach(() => {
    originalFetch = globalThis.fetch
    activeProject.set('my-project')
    messagesByChannel.set({ 'my-project': [] })
  })

  afterEach(() => {
    globalThis.fetch = originalFetch
    activeProject.set(null)
  })

  it('buckets a message with no channel field under activeProject, not midtown', async () => {
    globalThis.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => [
        { id: 1, content: 'hello', from: 'lead', timestamp: '2026-01-01T00:00:00Z' },
      ],
    })

    await fetchHistory()

    const store = get(messagesByChannel)
    expect(store['my-project']).toHaveLength(1)
    expect(store['midtown']).toBeUndefined()
  })
})

describe('handleUpdate channel_message — channelless messages route to activeProject', () => {
  beforeEach(() => {
    activeProject.set('my-project')
    messagesByChannel.set({ 'my-project': [] })
    threadData.set(null)
  })

  afterEach(() => {
    activeProject.set(null)
  })

  it('routes a message with no channel field to activeProject, not midtown', () => {
    handleUpdate({
      type: 'channel_message',
      data: { id: 'msg-1', from: 'lead', content: 'hello', timestamp: '2026-01-01T00:00:00Z' },
    })

    const store = get(messagesByChannel)
    expect(store['my-project']).toHaveLength(1)
    expect(store['midtown']).toBeUndefined()
  })
})

describe('handleUpdate universal_items — null channel uses activeProject, not hardcoded midtown', () => {
  const sampleItem = { status: 'Completed', content: [] }

  beforeEach(() => {
    vi.useFakeTimers()
    agentToolItems.set({})
    activeProject.set('my-project')
    messagesByChannel.set({ 'my-project': [] })
  })

  afterEach(() => {
    vi.useRealTimers()
    activeProject.set(null)
  })

  it('stores tool items under activeProject when channel is null', () => {
    handleUpdate({
      type: 'universal_items',
      data: { channel: null, agent_name: 'lead', items: [sampleItem] },
    })

    const items = get(agentToolItems)
    expect(items['my-project']).toHaveLength(1)
    expect(items['midtown']).toBeUndefined()
  })
})

describe('handleUpdate universal_items — thread_parent_id routes to threadToolItems', () => {
  const sampleItem = {
    status: 'InProgress',
    content: [{ ToolCall: { call_id: 'tc-1', name: 'Read', semantic_header: 'Read file.txt' } }],
  }

  beforeEach(() => {
    vi.useFakeTimers()
    agentToolItems.set({})
    threadToolItems.set({})
    activeProject.set('midtown')
    messagesByChannel.set({ midtown: [], web: [] })
  })

  afterEach(() => {
    vi.useRealTimers()
    activeProject.set(null)
  })

  it('stores items in threadToolItems when thread_parent_id is present', () => {
    handleUpdate({
      type: 'universal_items',
      data: { channel: 'web', agent_name: 'fork-abcd', thread_parent_id: 'msg-9999', items: [sampleItem] },
    })

    // Should be in threadToolItems, NOT agentToolItems
    expect(get(threadToolItems)['msg-9999']).toHaveLength(1)
    expect(get(agentToolItems)['web']).toBeUndefined()
  })

  it('stores items in agentToolItems when thread_parent_id is absent', () => {
    handleUpdate({
      type: 'universal_items',
      data: { channel: 'web', agent_name: 'web', items: [sampleItem] },
    })

    expect(get(agentToolItems)['web']).toHaveLength(1)
    expect(get(threadToolItems)).toEqual({})
  })

  it('clears thread tool items after delay when a thread reply arrives', () => {
    handleUpdate({
      type: 'universal_items',
      data: { channel: 'web', agent_name: 'fork-abcd', thread_parent_id: 'msg-9999', items: [sampleItem] },
    })
    expect(get(threadToolItems)['msg-9999']).toHaveLength(1)

    // Fork posts a reply in the thread
    handleUpdate({
      type: 'channel_message',
      data: { id: 'reply-1', from: 'fork-abcd', content: 'Done!', channel: 'web', thread_parent_id: 'msg-9999', timestamp: '2026-01-01T00:00:00Z' },
    })

    // Items still present before delay
    expect(get(threadToolItems)['msg-9999']).toHaveLength(1)

    // After delay, items are cleared
    vi.advanceTimersByTime(5000)
    expect(get(threadToolItems)['msg-9999']).toBeUndefined()
  })

  it('cancels thread clear timeout when new thread tool activity arrives', () => {
    handleUpdate({
      type: 'universal_items',
      data: { channel: 'web', agent_name: 'fork-abcd', thread_parent_id: 'msg-9999', items: [sampleItem] },
    })

    // Fork posts a reply — schedules delayed clear
    handleUpdate({
      type: 'channel_message',
      data: { id: 'reply-1', from: 'fork-abcd', content: 'status', channel: 'web', thread_parent_id: 'msg-9999', timestamp: '2026-01-01T00:00:00Z' },
    })

    // New tool activity arrives before delay — cancels the pending clear
    vi.advanceTimersByTime(2000)
    const newItem = { ...sampleItem, content: [{ ToolCall: { call_id: 'tc-2', name: 'Edit', semantic_header: 'Edit app.js' } }] }
    handleUpdate({
      type: 'universal_items',
      data: { channel: 'web', agent_name: 'fork-abcd', thread_parent_id: 'msg-9999', items: [newItem] },
    })

    // Advance past original delay — items should still be present
    vi.advanceTimersByTime(3000)
    expect(get(threadToolItems)['msg-9999']).toHaveLength(2)
  })
})
