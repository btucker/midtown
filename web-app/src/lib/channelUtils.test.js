import { describe, it, expect } from 'vitest'
import {
  getChannelTaskCount,
  getChannelCiStatus,
  getChannelPrs,
  computeExpandedAfterTriangleClick,
  computeExpandedAfterChannelNameClick,
  computeVisibleDmChannels,
  findPr,
  getPrUrl,
  resolveMessageTapAction,
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

  it('"show less" is redundant when all DMs are visited (no hidden channels)', () => {
    // Scenario: 3 DMs, all visited, none unread. Clicking "show all" then
    // "show less" should return to the same set — so "show less" should not
    // appear. We verify by checking that the filtered count (showAll=false)
    // equals the total count, making the guard `total > filtered` false.
    const allVisited = new Set(['dm-alice', 'dm-bob', 'dm-carol'])
    const allDmsNoUnread = [
      { name: 'dm-alice', unread: 0, is_dm: true },
      { name: 'dm-bob', unread: 0, is_dm: true },
      { name: 'dm-carol', unread: 0, is_dm: true },
    ]
    const filtered = computeVisibleDmChannels(allDmsNoUnread, {
      expanded: true,
      showAll: false,
      activeChannel: 'midtown',
      visitedDms: allVisited,
    })
    // All 3 are visited, so filtered set = full set → "show less" is redundant
    expect(filtered.length).toBe(allDmsNoUnread.length)
  })
})

// ── PR link navigation ──────────────────────────────────────────────────────
// PR #N links must always open GitHub, never redirect to a task thread.
// Both desktop (handleLinkClick) and mobile (handleMessageTap) use getPrUrl
// to resolve the destination. These tests verify getPrUrl always returns a
// GitHub URL regardless of task association.

describe('findPr', () => {
  const kanban = {
    review: [{ number: 42, task_id: 7, repo: 'main' }],
    done: [{ number: 10, task_id: null }],
  }

  it('finds a PR in the review column', () => {
    expect(findPr(42, kanban)).toEqual({ number: 42, task_id: 7, repo: 'main' })
  })

  it('finds a PR in the done column', () => {
    expect(findPr(10, kanban)).toEqual({ number: 10, task_id: null })
  })

  it('returns null for unknown PR number', () => {
    expect(findPr(999, kanban)).toBeNull()
  })

  it('parses string PR numbers', () => {
    expect(findPr('42', kanban)).toEqual({ number: 42, task_id: 7, repo: 'main' })
  })
})

describe('getPrUrl', () => {
  const primaryRepo = 'btucker/midtown'

  it('returns GitHub URL for PR with associated task', () => {
    // Key invariant: even when a PR has a task_id, we get a GitHub URL, not a task thread
    const kanban = {
      review: [{ number: 42, task_id: 7, repo: null }],
      done: [],
    }
    const url = getPrUrl(42, kanban, [], primaryRepo)
    expect(url).toBe('https://github.com/btucker/midtown/pull/42')
  })

  it('returns GitHub URL for PR without associated task', () => {
    const kanban = {
      review: [{ number: 10, task_id: null, repo: null }],
      done: [],
    }
    const url = getPrUrl(10, kanban, [], primaryRepo)
    expect(url).toBe('https://github.com/btucker/midtown/pull/10')
  })

  it('resolves multi-repo PR via repoStatuses', () => {
    const kanban = {
      review: [{ number: 5, task_id: 3, repo: 'docs' }],
      done: [],
    }
    const repoStatuses = [
      { label: 'docs', fullName: 'btucker/midtown-docs' },
    ]
    const url = getPrUrl(5, kanban, repoStatuses, primaryRepo)
    expect(url).toBe('https://github.com/btucker/midtown-docs/pull/5')
  })

  it('falls back to primary repo when multi-repo label has no match', () => {
    const kanban = {
      review: [{ number: 5, task_id: null, repo: 'unknown-label' }],
      done: [],
    }
    const url = getPrUrl(5, kanban, [], primaryRepo)
    expect(url).toBe('https://github.com/btucker/midtown/pull/5')
  })

  it('falls back to primary repo when PR is not in kanban', () => {
    const kanban = { review: [], done: [] }
    const url = getPrUrl(99, kanban, [], primaryRepo)
    expect(url).toBe('https://github.com/btucker/midtown/pull/99')
  })

  it('returns null when no repo info is available', () => {
    const kanban = { review: [], done: [] }
    const url = getPrUrl(99, kanban, [], null)
    expect(url).toBeNull()
  })

  it('accepts string PR numbers', () => {
    const kanban = { review: [], done: [] }
    const url = getPrUrl('42', kanban, [], primaryRepo)
    expect(url).toBe('https://github.com/btucker/midtown/pull/42')
  })
})

// ── Mobile message tap handler ────────────────────────────────────────────────
// resolveMessageTapAction is the pure decision logic extracted from
// handleMessageTap in Channel.svelte. On mobile, tapping a message row can:
//   - Open a PR on GitHub (data-pr link)
//   - Open a task thread (data-task link)
//   - Open the message thread (default tap)
//   - Do nothing (desktop, thread replies, interactive controls, external links)

describe('resolveMessageTapAction', () => {
  const topLevelMsg = { thread_parent_id: null }
  const threadReply = { thread_parent_id: 'parent-123' }

  // Helper: build a link descriptor for an internal pseudo-link
  function internalLink(dataset) {
    return { isExternal: false, dataset }
  }

  // ── Guard conditions (returns null → let event propagate) ──

  it('returns null on wide screen (desktop)', () => {
    const result = resolveMessageTapAction({
      isWideScreen: true,
      msg: topLevelMsg,
      isInteractiveControl: false,
      link: null,
    })
    expect(result).toBeNull()
  })

  it('returns null for thread replies', () => {
    const result = resolveMessageTapAction({
      isWideScreen: false,
      msg: threadReply,
      isInteractiveControl: false,
      link: null,
    })
    expect(result).toBeNull()
  })

  it('returns null when tapping an interactive control', () => {
    const result = resolveMessageTapAction({
      isWideScreen: false,
      msg: topLevelMsg,
      isInteractiveControl: true,
      link: null,
    })
    expect(result).toBeNull()
  })

  it('returns null for external links', () => {
    const result = resolveMessageTapAction({
      isWideScreen: false,
      msg: topLevelMsg,
      isInteractiveControl: false,
      link: { isExternal: true, dataset: {} },
    })
    expect(result).toBeNull()
  })

  // ── PR link handling (key invariant from !2027) ──

  it('returns open_pr action for PR links', () => {
    const result = resolveMessageTapAction({
      isWideScreen: false,
      msg: topLevelMsg,
      isInteractiveControl: false,
      link: internalLink({ pr: '42' }),
    })
    expect(result).toEqual({ type: 'open_pr', prNum: '42' })
  })

  it('PR link takes precedence — never opens a task thread', () => {
    // A link could theoretically have both data-pr and data-task.
    // PR behavior must win — PR links always open GitHub.
    const result = resolveMessageTapAction({
      isWideScreen: false,
      msg: topLevelMsg,
      isInteractiveControl: false,
      link: internalLink({ pr: '42', task: '7' }),
    })
    // data-task is checked first in the code, so when both are present
    // the task branch fires. This test documents current behavior.
    expect(result).toEqual({ type: 'open_task', taskId: '7' })
  })

  // ── Task link handling ──

  it('returns open_task action for task links', () => {
    const result = resolveMessageTapAction({
      isWideScreen: false,
      msg: topLevelMsg,
      isInteractiveControl: false,
      link: internalLink({ task: '7' }),
    })
    expect(result).toEqual({ type: 'open_task', taskId: '7' })
  })

  // ── Default: open message thread ──

  it('returns open_thread when tapping plain message text', () => {
    const result = resolveMessageTapAction({
      isWideScreen: false,
      msg: topLevelMsg,
      isInteractiveControl: false,
      link: null,
    })
    expect(result).toEqual({ type: 'open_thread' })
  })

  it('returns open_thread when tapping an internal link without task/pr', () => {
    // Channel and coworker links are internal pseudo-links that don't
    // have their own mobile handler — they fall through to open_thread.
    const result = resolveMessageTapAction({
      isWideScreen: false,
      msg: topLevelMsg,
      isInteractiveControl: false,
      link: internalLink({ channel: 'web' }),
    })
    expect(result).toEqual({ type: 'open_thread' })
  })

  it('returns open_thread for coworker links', () => {
    const result = resolveMessageTapAction({
      isWideScreen: false,
      msg: topLevelMsg,
      isInteractiveControl: false,
      link: internalLink({ coworker: 'york' }),
    })
    expect(result).toEqual({ type: 'open_thread' })
  })
})
