// Shared utilities for channel filtering and task counting

/**
 * Build a map of task_id → channel from the kanban task lists.
 * Used to look up a PR's channel via its associated task.
 */
function buildTaskChannelMap(kanban) {
  const map = new Map()
  for (const task of kanban.inProgress) {
    if (task.id != null && task.channel) {
      map.set(String(task.id), task.channel)
    }
  }
  for (const task of kanban.backlog) {
    if (task.id != null && task.channel) {
      map.set(String(task.id), task.channel)
    }
  }
  return map
}

/**
 * Get the channel for a PR by looking up its task_id in the task channel map.
 * Returns the channel name or null if the PR has no associated task/channel.
 */
function getPrChannel(pr, taskChannelMap) {
  if (pr.task_id != null) {
    return taskChannelMap.get(String(pr.task_id)) || null
  }
  return null
}

/**
 * Filter PRs by channel, using task_id → channel lookup.
 * PRs without a task or without a channel assignment only appear in the main channel.
 */
function filterPrsByChannel(prs, channelName, taskChannelMap) {
  return prs.filter((pr) => getPrChannel(pr, taskChannelMap) === channelName)
}

/**
 * Get task count for a channel, filtering by the task's channel field.
 * Main channel shows all tasks, topic channels filter by channel field.
 *
 * This matches the TUI implementation which groups tasks by task.channel.
 */
export function getChannelTaskCount(channelName, kanban) {
  // Tasks with no channel field default to the main channel (matches TUI's unwrap_or(main_channel))
  const filterTasks = (list) => {
    if (channelName === 'midtown') {
      return list.filter((task) => !task.channel || task.channel === 'midtown')
    }
    return list.filter((task) => task.channel === channelName)
  }

  // For PRs, look up channel via task_id → channel map (consistent with task filtering).
  // PRs with no task_id default to the main channel.
  const taskChannelMap = buildTaskChannelMap(kanban)
  const filterPrs = (prs) => {
    if (channelName === 'midtown') {
      return prs.filter((pr) => {
        const ch = getPrChannel(pr, taskChannelMap)
        return ch === null || ch === 'midtown'
      })
    }
    return filterPrsByChannel(prs, channelName, taskChannelMap)
  }

  return {
    inProgress: filterTasks(kanban.inProgress).length,
    pending: filterTasks(kanban.backlog).length,
    review: filterPrs(kanban.review).length,
  }
}

/**
 * Get CI status for a channel based on its PRs.
 * Returns 'failed', 'pending', 'passed', or null.
 */
export function getChannelCiStatus(channelName, kanban) {
  if (channelName === 'midtown') {
    // Main channel considers all PRs
    if (kanban.review.length === 0) return null
    if (kanban.review.some((pr) => pr.status === 'ci_failed')) return 'failed'
    if (kanban.review.some((pr) => pr.status === 'ci_pending')) return 'pending'
    if (kanban.review.every((pr) => pr.status === 'ci_passed' || pr.status === 'approved')) return 'passed'
    return null
  }

  const taskChannelMap = buildTaskChannelMap(kanban)
  const channelPrs = filterPrsByChannel(kanban.review, channelName, taskChannelMap)
  if (channelPrs.length === 0) return null

  // Check if any PR has failing CI
  if (channelPrs.some((pr) => pr.status === 'ci_failed')) return 'failed'
  if (channelPrs.some((pr) => pr.status === 'ci_pending')) return 'pending'
  if (channelPrs.every((pr) => pr.status === 'ci_passed' || pr.status === 'approved')) return 'passed'
  return null
}

/**
 * Returns true if a channel has any active tasks (in-progress or pending).
 * Used to determine whether to auto-expand the task list on channel select.
 */
export function getChannelHasActiveTasks(channelName, kanban) {
  const counts = getChannelTaskCount(channelName, kanban)
  return counts.inProgress > 0 || counts.pending > 0
}

/**
 * Compute the expanded channels set after clicking the triangle (▶/▼) on channelName.
 * The triangle always toggles: collapsed → expanded, expanded → collapsed.
 * Returns a new Set; does not mutate the input.
 */
export function computeExpandedAfterTriangleClick(channelName, expandedChannels) {
  const next = new Set(expandedChannels)
  if (next.has(channelName)) {
    next.delete(channelName)
  } else {
    next.add(channelName)
  }
  return next
}

/**
 * Compute the expanded channels set after clicking the channel name.
 * - Switching to an inactive channel: auto-expand if it has active tasks.
 * - Re-clicking the already-active channel: toggle expand/collapse.
 * Returns a new Set; does not mutate the input.
 */
export function computeExpandedAfterChannelNameClick(channelName, expandedChannels, activeChannel, kanban) {
  const next = new Set(expandedChannels)
  if (channelName === activeChannel) {
    if (next.has(channelName)) {
      next.delete(channelName)
    } else {
      next.add(channelName)
    }
  } else if (getChannelHasActiveTasks(channelName, kanban)) {
    next.add(channelName)
  }
  return next
}

/**
 * Get active PRs for a channel, using task_id → channel lookup.
 * Main channel shows all PRs, topic channels filter by task channel.
 */
export function getChannelPrs(channelName, kanban) {
  const taskChannelMap = buildTaskChannelMap(kanban)
  if (channelName === 'midtown') {
    // Main channel shows PRs with no task, or whose task has no channel (or channel='midtown')
    return kanban.review.filter((pr) => {
      const ch = getPrChannel(pr, taskChannelMap)
      return ch === null || ch === 'midtown'
    })
  }
  return filterPrsByChannel(kanban.review, channelName, taskChannelMap)
}
