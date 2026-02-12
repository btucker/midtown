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
  if (channelName === 'midtown') {
    // Main channel shows all tasks, including those with no explicit channel assignment
    return {
      inProgress: kanban.inProgress.length,
      pending: kanban.backlog.length,
      review: kanban.review.length,
    }
  }

  // Topic channels filter by the task's channel field
  const filterTasks = (list) => list.filter((task) => task.channel === channelName)

  // For PRs, look up channel via task_id → channel map (consistent with task filtering)
  const taskChannelMap = buildTaskChannelMap(kanban)

  return {
    inProgress: filterTasks(kanban.inProgress).length,
    pending: filterTasks(kanban.backlog).length,
    review: filterPrsByChannel(kanban.review, channelName, taskChannelMap).length,
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
 * Get active PRs for a channel, using task_id → channel lookup.
 * Main channel shows all PRs, topic channels filter by task channel.
 */
export function getChannelPrs(channelName, kanban) {
  if (channelName === 'midtown') {
    return kanban.review
  }
  const taskChannelMap = buildTaskChannelMap(kanban)
  return filterPrsByChannel(kanban.review, channelName, taskChannelMap)
}
