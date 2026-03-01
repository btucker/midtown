import { describe, it, expect } from 'vitest'
import {
  getChannelTaskCount,
  getChannelCiStatus,
  getChannelPrs,
  computeExpandedAfterTriangleClick,
  computeExpandedAfterChannelNameClick,
  computeVisibleDmChannels,
} from './channelUtils.js'

describe('getChannelTaskCount', () => {
  const mockKanban = {
    // Tasks use explicit `channel` field matching the backend structure
    inProgress: [
      { id: 101, title: 'Add JWT', channel: 'auth-refactor' },
      { id: 102, title: 'Dark mode', channel: 'ui-improvements' },
      { id: 103, title: 'Other task', channel: null }, // No explicit channel = midtown default
    ],
    backlog: [
      { id: 201, title: 'Update tests', channel: 'auth-refactor' },
      { id: 202, title: 'Unrelated pending task', channel: null },
    ],
    // PRs reference tasks via task_id — channel resolved via task lookup
    review: [
      { task_id: 101, task_name: 'Add JWT' },
    ],
  }

  it('returns only midtown-owned tasks for midtown channel', () => {
    // mockKanban has 3 inProgress: channel='auth-refactor', 'ui-improvements', null
    // and 2 pending: channel='auth-refactor', null
    // midtown should only see tasks with no channel (or channel==='midtown')
    const counts = getChannelTaskCount('midtown', mockKanban)
    expect(counts).toEqual({
      inProgress: 1, // only the channel:null task
      pending: 1,    // only the channel:null task
      review: 0,     // PR's task is in 'auth-refactor', not midtown
    })
  })

  it('does not show tasks assigned to other channels in midtown', () => {
    // Regression: tasks with an explicit channel field were appearing in both
    // midtown AND their assigned channel, causing duplicates in the sidebar.
    const kanban = {
      inProgress: [
        { id: 1778, title: 'Auth profile pool', channel: 'multi-platform' },
        { id: 1779, title: 'Auth profile pool: integrati...', channel: 'multi-platform' },
        { id: 1780, title: 'Midtown task', channel: null },
      ],
      backlog: [
        { id: 1781, title: 'Another multi-platform task', channel: 'multi-platform' },
        { id: 1782, title: 'Unassigned task', channel: null },
      ],
      review: [],
    }
    const counts = getChannelTaskCount('midtown', kanban)
    // Only tasks with no channel (or channel==='midtown') should appear
    expect(counts.inProgress).toBe(1)
    expect(counts.pending).toBe(1)
  })

  it('filters tasks by channel field for topic channels', () => {
    const counts = getChannelTaskCount('auth-refactor', mockKanban)
    expect(counts).toEqual({
      inProgress: 1,
      pending: 1,
      review: 1,
    })
  })

  it('returns zero counts for channel with no matching tasks', () => {
    const counts = getChannelTaskCount('nonexistent', mockKanban)
    expect(counts).toEqual({
      inProgress: 0,
      pending: 0,
      review: 0,
    })
  })

  it('groups PRs by task channel, not task_name text', () => {
    // PR's task_name does NOT contain the channel name,
    // but the corresponding task has the correct channel field
    const kanban = {
      inProgress: [
        { id: 50, title: 'Implement feature', channel: 'my-channel' },
      ],
      backlog: [],
      review: [
        { task_id: 50, task_name: 'Implement feature' },
      ],
    }
    const counts = getChannelTaskCount('my-channel', kanban)
    expect(counts.review).toBe(1)
  })

  it('PRs without task_id only appear in main channel', () => {
    const kanban = {
      inProgress: [
        { id: 50, title: 'Task', channel: 'my-channel' },
      ],
      backlog: [],
      review: [
        { task_name: 'Orphan PR' }, // no task_id
      ],
    }
    const mainCounts = getChannelTaskCount('midtown', kanban)
    const topicCounts = getChannelTaskCount('my-channel', kanban)
    expect(mainCounts.review).toBe(1)
    expect(topicCounts.review).toBe(0)
  })
})

describe('getChannelCiStatus', () => {
  it('returns null when no PRs match', () => {
    const mockKanban = { inProgress: [], backlog: [], review: [] }
    expect(getChannelCiStatus('auth', mockKanban)).toBe(null)
  })

  it('returns "failed" if any PR has ci_failed', () => {
    const mockKanban = {
      inProgress: [
        { id: 1, title: 'Task 1', channel: 'auth' },
        { id: 2, title: 'Task 2', channel: 'auth' },
      ],
      backlog: [],
      review: [
        { task_id: 1, task_name: 'Task 1', status: 'ci_passed' },
        { task_id: 2, task_name: 'Task 2', status: 'ci_failed' },
      ],
    }
    expect(getChannelCiStatus('auth', mockKanban)).toBe('failed')
  })

  it('returns "pending" if any PR has ci_pending and none failed', () => {
    const mockKanban = {
      inProgress: [
        { id: 1, title: 'Task 1', channel: 'auth' },
        { id: 2, title: 'Task 2', channel: 'auth' },
      ],
      backlog: [],
      review: [
        { task_id: 1, task_name: 'Task 1', status: 'ci_passed' },
        { task_id: 2, task_name: 'Task 2', status: 'ci_pending' },
      ],
    }
    expect(getChannelCiStatus('auth', mockKanban)).toBe('pending')
  })

  it('returns "passed" if all PRs passed or approved', () => {
    const mockKanban = {
      inProgress: [
        { id: 1, title: 'Task 1', channel: 'auth' },
        { id: 2, title: 'Task 2', channel: 'auth' },
      ],
      backlog: [],
      review: [
        { task_id: 1, task_name: 'Task 1', status: 'ci_passed' },
        { task_id: 2, task_name: 'Task 2', status: 'approved' },
      ],
    }
    expect(getChannelCiStatus('auth', mockKanban)).toBe('passed')
  })

  it('returns status for all PRs in midtown channel', () => {
    const mockKanban = {
      inProgress: [],
      backlog: [],
      review: [
        { task_id: 1, task_name: 'Task 1', status: 'ci_failed' },
      ],
    }
    expect(getChannelCiStatus('midtown', mockKanban)).toBe('failed')
  })
})

describe('getChannelPrs', () => {
  const mockKanban = {
    inProgress: [
      { id: 101, title: 'Add JWT', channel: 'auth-refactor' },
      { id: 102, title: 'Dark mode', channel: 'ui-improvements' },
    ],
    backlog: [],
    review: [
      { task_id: 101, task_name: 'Add JWT', number: 42 },
      { task_id: 102, task_name: 'Dark mode', number: 43 },
      { task_name: 'Other PR', number: 44 }, // no task_id
    ],
  }

  it('returns only midtown-owned PRs for midtown channel', () => {
    // PR #44 has no task_id → goes to midtown
    // PR #42 has task_id:101 (channel:'auth-refactor') → not midtown
    // PR #43 has task_id:102 (channel:'ui-improvements') → not midtown
    const prs = getChannelPrs('midtown', mockKanban)
    expect(prs).toHaveLength(1)
    expect(prs[0].number).toBe(44)
  })

  it('filters PRs by task channel for topic channels', () => {
    const prs = getChannelPrs('auth-refactor', mockKanban)
    expect(prs).toHaveLength(1)
    expect(prs[0].number).toBe(42)
  })

  it('returns empty array for channel with no matching PRs', () => {
    const prs = getChannelPrs('nonexistent', mockKanban)
    expect(prs).toEqual([])
  })

  it('groups PRs by task channel, not task_name text', () => {
    // PR task_name does NOT mention the channel name
    const kanban = {
      inProgress: [
        { id: 50, title: 'Implement feature', channel: 'special-channel' },
      ],
      backlog: [],
      review: [
        { task_id: 50, task_name: 'Implement feature', number: 99 },
      ],
    }
    const prs = getChannelPrs('special-channel', kanban)
    expect(prs).toHaveLength(1)
    expect(prs[0].number).toBe(99)
  })
})

describe('computeExpandedAfterTriangleClick', () => {
  it('expands a collapsed channel', () => {
    const result = computeExpandedAfterTriangleClick('web', new Set())
    expect(result.has('web')).toBe(true)
  })

  it('collapses an expanded channel', () => {
    const result = computeExpandedAfterTriangleClick('web', new Set(['web']))
    expect(result.has('web')).toBe(false)
  })

  it('does not affect other channels', () => {
    const result = computeExpandedAfterTriangleClick('web', new Set(['auth', 'web']))
    expect(result.has('auth')).toBe(true)
    expect(result.has('web')).toBe(false)
  })

  it('returns a new set (does not mutate the original)', () => {
    const original = new Set(['web'])
    const result = computeExpandedAfterTriangleClick('web', original)
    expect(original.has('web')).toBe(true)
    expect(result.has('web')).toBe(false)
  })
})

describe('computeExpandedAfterChannelNameClick', () => {
  const mockKanban = {
    inProgress: [{ id: 1, title: 'Build feature', channel: 'web' }],
    backlog: [],
    review: [],
  }

  it('auto-expands when switching to inactive channel with active tasks', () => {
    const result = computeExpandedAfterChannelNameClick('web', new Set(), 'midtown', mockKanban)
    expect(result.has('web')).toBe(true)
  })

  it('does not expand when switching to inactive channel without tasks', () => {
    const result = computeExpandedAfterChannelNameClick('empty', new Set(), 'midtown', mockKanban)
    expect(result.has('empty')).toBe(false)
  })

  it('expands already-active collapsed channel (toggle)', () => {
    const result = computeExpandedAfterChannelNameClick('web', new Set(), 'web', mockKanban)
    expect(result.has('web')).toBe(true)
  })

  it('collapses already-active expanded channel (toggle)', () => {
    const result = computeExpandedAfterChannelNameClick('web', new Set(['web']), 'web', mockKanban)
    expect(result.has('web')).toBe(false)
  })

  it('keeps expanded state when switching to already-expanded inactive channel', () => {
    const result = computeExpandedAfterChannelNameClick('web', new Set(['web']), 'midtown', mockKanban)
    expect(result.has('web')).toBe(true)
  })

  it('does not collapse other expanded channels when switching', () => {
    const result = computeExpandedAfterChannelNameClick('web', new Set(['auth']), 'midtown', mockKanban)
    expect(result.has('auth')).toBe(true)
    expect(result.has('web')).toBe(true)
  })
})

describe('computeVisibleDmChannels', () => {
  const dmChannels = [
    { name: 'dm-alice', unread: 2, is_dm: true },
    { name: 'dm-bob', unread: 0, is_dm: true },
    { name: 'dm-carol', unread: 0, is_dm: true },
  ]

  it('returns empty array when section is collapsed', () => {
    const result = computeVisibleDmChannels(dmChannels, {
      expanded: false,
      showAll: false,
      activeChannel: 'dm-alice',
      visitedDms: new Set(),
    })
    expect(result).toEqual([])
  })

  it('returns all DMs when showAll is true', () => {
    const result = computeVisibleDmChannels(dmChannels, {
      expanded: true,
      showAll: true,
      activeChannel: 'midtown',
      visitedDms: new Set(),
    })
    expect(result).toEqual(dmChannels)
  })

  it('shows unread DMs when expanded', () => {
    const result = computeVisibleDmChannels(dmChannels, {
      expanded: true,
      showAll: false,
      activeChannel: 'midtown',
      visitedDms: new Set(),
    })
    expect(result.map((ch) => ch.name)).toEqual(['dm-alice'])
  })

  it('shows the active DM even if it has no unread messages', () => {
    const result = computeVisibleDmChannels(dmChannels, {
      expanded: true,
      showAll: false,
      activeChannel: 'dm-bob',
      visitedDms: new Set(),
    })
    expect(result.map((ch) => ch.name)).toContain('dm-bob')
  })

  it('keeps a visited DM visible after navigating away (Bug #2 regression)', () => {
    // Scenario: user opened dm-bob (visited), then switched to a regular channel.
    // dm-bob has unread=0 and is not activeChannel — but it was visited, so it
    // should remain visible.
    const result = computeVisibleDmChannels(dmChannels, {
      expanded: true,
      showAll: false,
      activeChannel: 'midtown',
      visitedDms: new Set(['dm-bob']),
    })
    expect(result.map((ch) => ch.name)).toContain('dm-bob')
  })

  it('shows unread + active + visited DMs together', () => {
    const result = computeVisibleDmChannels(dmChannels, {
      expanded: true,
      showAll: false,
      activeChannel: 'dm-carol',
      visitedDms: new Set(['dm-bob']),
    })
    // dm-alice: unread > 0, dm-bob: visited, dm-carol: active
    expect(result.map((ch) => ch.name)).toEqual(['dm-alice', 'dm-bob', 'dm-carol'])
  })
})
