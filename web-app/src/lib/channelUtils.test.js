import { describe, it, expect } from 'vitest'
import { getChannelTaskCount, getChannelCiStatus, getChannelPrs } from './channelUtils.js'

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

  it('returns all tasks for midtown channel', () => {
    const counts = getChannelTaskCount('midtown', mockKanban)
    expect(counts).toEqual({
      inProgress: 3,
      pending: 2,
      review: 1,
    })
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

  it('returns all PRs for midtown channel', () => {
    const prs = getChannelPrs('midtown', mockKanban)
    expect(prs).toHaveLength(3)
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
