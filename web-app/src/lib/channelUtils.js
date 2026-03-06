// Shared utilities for channel filtering, task counting, and sidebar expansion state

/**
 * Build a map of task_id → channel from the kanban task lists.
 * Used to look up a PR's channel via its associated task.
 */
function buildTaskChannelMap(kanban) {
	const map = new Map();
	for (const task of kanban.inProgress) {
		if (task.id != null && task.channel) {
			map.set(String(task.id), task.channel);
		}
	}
	for (const task of kanban.backlog) {
		if (task.id != null && task.channel) {
			map.set(String(task.id), task.channel);
		}
	}
	return map;
}

/**
 * Get the channel for a PR by looking up its task_id in the task channel map.
 * Returns the channel name or null if the PR has no associated task/channel.
 */
function getPrChannel(pr, taskChannelMap) {
	if (pr.task_id != null) {
		return taskChannelMap.get(String(pr.task_id)) || null;
	}
	return null;
}

/**
 * Filter PRs by channel, using task_id → channel lookup.
 * PRs without a task or without a channel assignment only appear in the main channel.
 */
function filterPrsByChannel(prs, channelName, taskChannelMap) {
	return prs.filter((pr) => getPrChannel(pr, taskChannelMap) === channelName);
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
		if (channelName === "midtown") {
			return list.filter((task) => !task.channel || task.channel === "midtown");
		}
		return list.filter((task) => task.channel === channelName);
	};

	// For PRs, look up channel via task_id → channel map (consistent with task filtering).
	// PRs with no task_id default to the main channel.
	const taskChannelMap = buildTaskChannelMap(kanban);
	const filterPrs = (prs) => {
		if (channelName === "midtown") {
			return prs.filter((pr) => {
				const ch = getPrChannel(pr, taskChannelMap);
				return ch === null || ch === "midtown";
			});
		}
		return filterPrsByChannel(prs, channelName, taskChannelMap);
	};

	return {
		inProgress: filterTasks(kanban.inProgress).length,
		pending: filterTasks(kanban.backlog).length,
		review: filterPrs(kanban.review).length,
	};
}

/**
 * Get CI status for a channel based on its PRs.
 * Returns 'failed', 'pending', 'passed', or null.
 */
export function getChannelCiStatus(channelName, kanban) {
	if (channelName === "midtown") {
		// Main channel considers all PRs
		if (kanban.review.length === 0) return null;
		if (kanban.review.some((pr) => pr.status === "ci_failed")) return "failed";
		if (kanban.review.some((pr) => pr.status === "ci_pending")) return "pending";
		if (kanban.review.every((pr) => pr.status === "ci_passed" || pr.status === "approved")) return "passed";
		return null;
	}

	const taskChannelMap = buildTaskChannelMap(kanban);
	const channelPrs = filterPrsByChannel(kanban.review, channelName, taskChannelMap);
	if (channelPrs.length === 0) return null;

	// Check if any PR has failing CI
	if (channelPrs.some((pr) => pr.status === "ci_failed")) return "failed";
	if (channelPrs.some((pr) => pr.status === "ci_pending")) return "pending";
	if (channelPrs.every((pr) => pr.status === "ci_passed" || pr.status === "approved")) return "passed";
	return null;
}

/**
 * Get the actual task objects for a channel, tagged with status.
 * Returns in-progress tasks first, then pending.
 */
export function getChannelTasks(channelName, kanban) {
	const filterTasks = (list) => {
		if (channelName === "midtown") {
			return list.filter((task) => !task.channel || task.channel === "midtown");
		}
		return list.filter((task) => task.channel === channelName);
	};
	return [
		...filterTasks(kanban.inProgress).map((t) => ({ ...t, status: "in_progress" })),
		...filterTasks(kanban.backlog).map((t) => ({ ...t, status: "pending" })),
	];
}

/**
 * Returns true if a channel has any active tasks (in-progress or pending).
 * Used to determine whether to auto-expand the task list on channel select.
 */
export function getChannelHasActiveTasks(channelName, kanban) {
	const counts = getChannelTaskCount(channelName, kanban);
	return counts.inProgress > 0 || counts.pending > 0;
}

/**
 * Compute the expanded channels set after clicking the triangle (▶/▼) on channelName.
 * The triangle always toggles: collapsed → expanded, expanded → collapsed.
 * Returns a new Set; does not mutate the input.
 */
export function computeExpandedAfterTriangleClick(channelName, expandedChannels) {
	const next = new Set(expandedChannels);
	if (next.has(channelName)) {
		next.delete(channelName);
	} else {
		next.add(channelName);
	}
	return next;
}

/**
 * Compute the expanded channels set after clicking the channel name.
 * - Switching to an inactive channel: auto-expand if it has active tasks or tracked threads.
 * - Re-clicking the already-active channel: toggle expand/collapse.
 * Returns a new Set; does not mutate the input.
 *
 * @param {object} opts - Optional { trackedThreads, taskThreadIds } for thread-aware expansion
 */
export function computeExpandedAfterChannelNameClick(channelName, expandedChannels, activeChannel, kanban, opts = {}) {
	const next = new Set(expandedChannels);
	if (channelName === activeChannel) {
		if (next.has(channelName)) {
			next.delete(channelName);
		} else {
			next.add(channelName);
		}
	} else {
		const hasTasks = getChannelHasActiveTasks(channelName, kanban);
		const hasThreads =
			opts.trackedThreads && opts.taskThreadIds
				? getChannelHasTrackedThreads(channelName, opts.trackedThreads, opts.taskThreadIds)
				: false;
		if (hasTasks || hasThreads) {
			next.add(channelName);
		}
	}
	return next;
}

/**
 * Compute the visible DM channels for the sidebar.
 * When collapsed, returns []. When expanded:
 *   - showAll → all DMs
 *   - otherwise → DMs with unread > 0, the active DM, or any previously visited DM
 *
 * "Visited" DMs are tracked so they don't vanish when unread drops to 0 and
 * the user collapses/re-expands the section.
 */
export function computeVisibleDmChannels(dmChannels, { expanded, showAll, activeChannel, visitedDms }) {
	if (!expanded) return [];
	if (showAll) return dmChannels;
	return dmChannels.filter((ch) => ch.unread > 0 || ch.name === activeChannel || visitedDms.has(ch.name));
}

// ── Thread sidebar utilities ──────────────────────────────────────────────────

/**
 * Build a Set of threadParentIds that are already represented by a task.
 * A thread is "task-backed" when any task's thread_id or message_id matches it.
 */
export function getTaskThreadIds(kanban) {
	const ids = new Set();
	for (const list of [kanban.inProgress, kanban.backlog]) {
		for (const task of list) {
			if (task.thread_id) ids.add(task.thread_id);
			if (task.message_id) ids.add(task.message_id);
		}
	}
	return ids;
}

/**
 * Build a Set of threadParentIds from completed tasks.
 */
export function getCompletedTaskThreadIds(kanban) {
	const ids = new Set();
	const completed = kanban.completedTasks || [];
	for (const task of completed) {
		if (task.thread_id) ids.add(task.thread_id);
		if (task.message_id) ids.add(task.message_id);
	}
	return ids;
}

/**
 * Get tracked threads for a channel, sorted by lastActivity (newest first).
 * Pure function — filters out active task-backed threads and returns their IDs
 * in `toClean` for the caller to handle cleanup separately (e.g. in a $effect).
 * Completed task threads are kept but marked with `completed: true`.
 *
 * @param {string} channelName
 * @param {object} tracked - $trackedThreads store value
 * @param {object} unreadCounts - $threadUnreadCounts store value
 * @param {Set} taskThreadIds - from getTaskThreadIds() (active tasks)
 * @param {Set} completedTaskThreadIds - from getCompletedTaskThreadIds()
 * @returns {{ threads: Array<{id, subject, lastActivity, replyCount, unread, completed}>, toClean: string[] }}
 */
export function getChannelThreads(
	channelName,
	tracked,
	unreadCounts,
	taskThreadIds,
	completedTaskThreadIds = new Set(),
) {
	const threads = [];
	const toClean = [];
	for (const [id, entry] of Object.entries(tracked)) {
		if (entry.channelName !== channelName) continue;
		if (taskThreadIds.has(id)) {
			toClean.push(id);
			continue;
		}
		threads.push({
			id,
			subject: entry.subject,
			fullText: entry.fullText || entry.subject,
			lastActivity: entry.lastActivity,
			replyCount: entry.replyCount || 0,
			unread: unreadCounts[id] || 0,
			completed: completedTaskThreadIds.has(id),
		});
	}
	threads.sort((a, b) => (b.lastActivity || "").localeCompare(a.lastActivity || ""));
	return { threads, toClean };
}

/**
 * Returns true if a channel has any tracked threads that aren't task-backed.
 */
export function getChannelHasTrackedThreads(channelName, tracked, taskThreadIds) {
	for (const [id, entry] of Object.entries(tracked)) {
		if (entry.channelName === channelName && !taskThreadIds.has(id)) return true;
	}
	return false;
}

/**
 * Find a PR by number across kanban columns that contain PR data.
 * PRs appear in 'review' (open) and 'done' (merged) columns.
 */
export function findPr(prNum, kanbanData) {
	const num = parseInt(prNum, 10);
	return kanbanData.review.find((p) => p.number === num) || kanbanData.done?.find((p) => p.number === num) || null;
}

/**
 * Build GitHub PR URL (multi-repo aware).
 * Looks up the PR in kanbanData to find its repo, then resolves via
 * repoStatuses. Falls back to the primary repo if no match is found.
 * Returns null if repo full name is unavailable.
 *
 * This always returns a GitHub URL regardless of whether the PR has
 * an associated task — PR links should always open GitHub.
 */
export function getPrUrl(prNum, kanbanData, repoStatuses, primaryRepoFullName) {
	const pr = findPr(prNum, kanbanData);
	// If the PR has a repo label, resolve it via repoStatuses (multi-repo)
	if (pr?.repo && repoStatuses.length > 0) {
		const info = repoStatuses.find((r) => r.label === pr.repo);
		if (info?.fullName) {
			return `https://github.com/${info.fullName}/pull/${prNum}`;
		}
	}
	// Fall back to the primary repo
	if (primaryRepoFullName) {
		return `https://github.com/${primaryRepoFullName}/pull/${prNum}`;
	}
	return null;
}

/**
 * Get active PRs for a channel, using task_id → channel lookup.
 * Main channel shows all PRs, topic channels filter by task channel.
 */
export function getChannelPrs(channelName, kanban) {
	const taskChannelMap = buildTaskChannelMap(kanban);
	if (channelName === "midtown") {
		// Main channel shows PRs with no task, or whose task has no channel (or channel='midtown')
		return kanban.review.filter((pr) => {
			const ch = getPrChannel(pr, taskChannelMap);
			return ch === null || ch === "midtown";
		});
	}
	return filterPrsByChannel(kanban.review, channelName, taskChannelMap);
}

// ── Mobile message tap handling ───────────────────────────────────────────────

/**
 * Determine the action for a mobile message tap.
 *
 * This is the pure decision logic extracted from Channel.svelte's
 * handleMessageTap. The Svelte component handles DOM specifics (closest(),
 * dataset extraction) and passes pre-processed inputs here.
 *
 * Returns null if the tap should be ignored (let the event propagate),
 * or an action descriptor: { type: 'open_task', taskId } |
 *   { type: 'open_pr', prNum } | { type: 'open_thread' }
 *
 * @param {object} opts
 * @param {boolean} opts.isWideScreen - true on desktop (skip mobile handling)
 * @param {object}  opts.msg - message object with thread_parent_id
 * @param {boolean} opts.isInteractiveControl - true if tap target is button/input/etc.
 * @param {object|null} opts.link - link info: { isExternal, dataset: { task, pr, channel, coworker } }
 */
export function resolveMessageTapAction({ isWideScreen, msg, isInteractiveControl, link }) {
	// Mobile-only affordance: skip on desktop or when tapping inside a thread
	if (isWideScreen || msg.thread_parent_id) return null;
	// Don't intercept taps on interactive controls
	if (isInteractiveControl) return null;
	// External links (no internal dataset) should follow their href normally
	if (link?.isExternal) return null;
	// PR links always open GitHub (checked before task — PR wins when both are present)
	if (link?.dataset?.pr) {
		return { type: "open_pr", prNum: link.dataset.pr };
	}
	// Task links open the task's thread (with task card)
	if (link?.dataset?.task) {
		return { type: "open_task", taskId: link.dataset.task };
	}
	// All other taps open the message's thread
	return { type: "open_thread" };
}
