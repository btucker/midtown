import { describe, it, expect } from 'vitest'
import { matchesChannel, getChannelTaskCount, getChannelCiStatus, getChannelPrs } from './channelUtils.js'

describe('matchesChannel', () => {
  it('matches channel name as whole word', () => {
    expect(matchesChannel('auth-refactor: Add JWT support', 'auth-refactor')).toBe(true)
    expect(matchesChannel('Working on auth-refactor module', 'auth-refactor')).toBe(true)
  })

  it('does not match partial words', () => {
    expect(matchesChannel('authentication: Add JWT support', 'auth')).toBe(false)
    expect(matchesChannel('Working on authorize module', 'auth')).toBe(false)
  })

  it('handles hyphenated channel names', () => {
    expect(matchesChannel('pr-42: Fix bug', 'pr-42')).toBe(true)
    expect(matchesChannel('task-created: pr-42', 'pr-42')).toBe(true)
    expect(matchesChannel('pr-421: Another task', 'pr-42')).toBe(false)
  })

  it('handles channel names with special regex characters', () => {
    expect(matchesChannel('test.channel: Task', 'test.channel')).toBe(true)
    expect(matchesChannel('Working on test+plus', 'test+plus')).toBe(true)
  })

  it('is case insensitive', () => {
    expect(matchesChannel('Auth-Refactor: Task', 'auth-refactor')).toBe(true)
    expect(matchesChannel('AUTH-REFACTOR: Task', 'auth-refactor')).toBe(true)
  })

  it('returns false for null/empty text', () => {
    expect(matchesChannel(null, 'auth')).toBe(false)
    expect(matchesChannel('', 'auth')).toBe(false)
    expect(matchesChannel(undefined, 'auth')).toBe(false)
  })
})

describe('getChannelTaskCount', () => {
  const mockKanban = {
    inProgress: [
      { title: 'auth-refactor: Add JWT' },
      { title: 'ui-improvements: Dark mode' },
      { title: 'Other task' },
    ],
    backlog: [
      { title: 'auth-refactor: Update tests' },
      { title: 'Unrelated pending task' },
    ],
    review: [
      { task_name: 'auth-refactor: Add JWT' },
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

  it('filters tasks by channel name for topic channels', () => {
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
})

describe('getChannelCiStatus', () => {
  it('returns null when no PRs match', () => {
    const mockKanban = { review: [] }
    expect(getChannelCiStatus('auth', mockKanban)).toBe(null)
  })

  it('returns "failed" if any PR has ci_failed', () => {
    const mockKanban = {
      review: [
        { task_name: 'auth: Task 1', status: 'ci_passed' },
        { task_name: 'auth: Task 2', status: 'ci_failed' },
      ],
    }
    expect(getChannelCiStatus('auth', mockKanban)).toBe('failed')
  })

  it('returns "pending" if any PR has ci_pending and none failed', () => {
    const mockKanban = {
      review: [
        { task_name: 'auth: Task 1', status: 'ci_passed' },
        { task_name: 'auth: Task 2', status: 'ci_pending' },
      ],
    }
    expect(getChannelCiStatus('auth', mockKanban)).toBe('pending')
  })

  it('returns "passed" if all PRs passed or approved', () => {
    const mockKanban = {
      review: [
        { task_name: 'auth: Task 1', status: 'ci_passed' },
        { task_name: 'auth: Task 2', status: 'approved' },
      ],
    }
    expect(getChannelCiStatus('auth', mockKanban)).toBe('passed')
  })
})

describe('getChannelPrs', () => {
  const mockKanban = {
    review: [
      { task_name: 'auth-refactor: Add JWT', number: 42 },
      { task_name: 'ui-improvements: Dark mode', number: 43 },
      { task_name: 'Other PR', number: 44 },
    ],
  }

  it('returns all PRs for midtown channel', () => {
    const prs = getChannelPrs('midtown', mockKanban)
    expect(prs).toHaveLength(3)
  })

  it('filters PRs by channel name for topic channels', () => {
    const prs = getChannelPrs('auth-refactor', mockKanban)
    expect(prs).toHaveLength(1)
    expect(prs[0].number).toBe(42)
  })

  it('returns empty array for channel with no matching PRs', () => {
    const prs = getChannelPrs('nonexistent', mockKanban)
    expect(prs).toEqual([])
  })
})
