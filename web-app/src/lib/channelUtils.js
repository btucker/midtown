// Shared utilities for channel filtering and task counting

/**
 * Match channel name as a whole word in task text (avoids "auth" matching "authentication").
 * Uses lookahead/lookbehind to ensure the channel name is not part of a hyphenated word.
 * For example, "pr" won't match "pr-42", but "pr-42" will match "pr-42".
 */
export function matchesChannel(text, channelName) {
  if (!text) return false
  // Escape special regex characters (excluding hyphen since it's handled in the pattern)
  const escaped = channelName.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  // Match only at boundaries that are NOT adjacent to word characters or hyphens
  // (?<![\w-]) - negative lookbehind: not preceded by word char or hyphen
  // (?![\w-]) - negative lookahead: not followed by word char or hyphen
  const pattern = new RegExp(`(?<![\\w-])${escaped}(?![\\w-])`, 'i')
  return pattern.test(text)
}

/**
 * Get task count for a channel, filtering by channel name as whole word in task description.
 * Main channel shows all tasks, topic channels filter by channel name.
 */
export function getChannelTaskCount(channelName, kanban) {
  if (channelName === 'midtown') {
    // Main channel shows all tasks
    return {
      inProgress: kanban.inProgress.length,
      pending: kanban.backlog.length,
      review: kanban.review.length,
    }
  }
  // Topic channels filter by channel name as whole word in task description
  const filter = (list) => list.filter((item) =>
    matchesChannel(item.title || item.task_name || '', channelName)
  )
  return {
    inProgress: filter(kanban.inProgress).length,
    pending: filter(kanban.backlog).length,
    review: filter(kanban.review).length,
  }
}

/**
 * Get CI status for a channel based on its PRs.
 * Returns 'failed', 'pending', 'passed', or null.
 */
export function getChannelCiStatus(channelName, kanban) {
  const channelPrs = kanban.review.filter((pr) => matchesChannel(pr.task_name, channelName))
  if (channelPrs.length === 0) return null

  // Check if any PR has failing CI
  if (channelPrs.some((pr) => pr.status === 'ci_failed')) return 'failed'
  if (channelPrs.some((pr) => pr.status === 'ci_pending')) return 'pending'
  if (channelPrs.every((pr) => pr.status === 'ci_passed' || pr.status === 'approved')) return 'passed'
  return null
}

/**
 * Get active PRs for a channel, filtering by channel name in task description.
 * Main channel shows all PRs, topic channels filter by channel name.
 */
export function getChannelPrs(channelName, kanban) {
  if (channelName === 'midtown') {
    return kanban.review
  }
  return kanban.review.filter((pr) => matchesChannel(pr.task_name, channelName))
}
