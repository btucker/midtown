// Shared utilities for channel filtering, task counting, and sidebar expansion state

import type {
	Channel,
	KanbanData,
	MergedPullRequest,
	Message,
	MultiRepoStatus,
	PullRequest,
	Task,
	ToolBlock,
	TrackedThread,
} from "./types.ts";

/**
 * Build a map of task_id → channel from the kanban task lists.
 * Used to look up a PR's channel via its associated task.
 */
function buildTaskChannelMap(kanban: KanbanData): Map<string, string> {
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
function getPrChannel(pr: PullRequest | MergedPullRequest, taskChannelMap: Map<string, string>): string | null {
	if ("task_id" in pr && pr.task_id != null) {
		return taskChannelMap.get(String(pr.task_id)) || null;
	}
	return null;
}

/**
 * Filter PRs by channel, using task_id → channel lookup.
 * PRs without a task or without a channel assignment only appear in the main channel.
 */
function filterPrsByChannel(
	prs: PullRequest[],
	channelName: string,
	taskChannelMap: Map<string, string>,
): PullRequest[] {
	return prs.filter((pr) => getPrChannel(pr, taskChannelMap) === channelName);
}

/**
 * Get task count for a channel, filtering by the task's channel field.
 * Main channel shows all tasks, topic channels filter by channel field.
 *
 * This matches the TUI implementation which groups tasks by task.channel.
 */
export function getChannelTaskCount(
	channelName: string,
	kanban: KanbanData,
): { inProgress: number; pending: number; review: number } {
	// Tasks with no channel field default to the main channel (matches TUI's unwrap_or(main_channel))
	const filterTasks = (list: Task[]) => {
		if (channelName === "midtown") {
			return list.filter((task) => !task.channel || task.channel === "midtown");
		}
		return list.filter((task) => task.channel === channelName);
	};

	// For PRs, look up channel via task_id → channel map (consistent with task filtering).
	// PRs with no task_id default to the main channel.
	const taskChannelMap = buildTaskChannelMap(kanban);
	const filterPrs = (prs: PullRequest[]) => {
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
export function getChannelCiStatus(channelName: string, kanban: KanbanData): string | null {
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
export function getChannelTasks(channelName: string, kanban: KanbanData): Task[] {
	const filterTasks = (list: Task[]) => {
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
export function getChannelHasActiveTasks(channelName: string, kanban: KanbanData): boolean {
	const counts = getChannelTaskCount(channelName, kanban);
	return counts.inProgress > 0 || counts.pending > 0;
}

/**
 * Compute the expanded channels set after clicking the triangle (▶/▼) on channelName.
 * The triangle always toggles: collapsed → expanded, expanded → collapsed.
 * Returns a new Set; does not mutate the input.
 */
export function computeExpandedAfterTriangleClick(channelName: string, expandedChannels: Set<string>): Set<string> {
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
 * @param {object} opts - Optional { trackedThreads, taskThreadIds, completedTaskThreadIds } for thread-aware expansion
 */
export function computeExpandedAfterChannelNameClick(
	channelName: string,
	expandedChannels: Set<string>,
	activeChannel: string | null,
	kanban: KanbanData,
	opts: {
		trackedThreads?: Record<string, TrackedThread>;
		taskThreadIds?: Set<string>;
		completedTaskThreadIds?: Set<string>;
	} = {},
): Set<string> {
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
				? getChannelHasTrackedThreads(channelName, opts.trackedThreads, opts.taskThreadIds, opts.completedTaskThreadIds)
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
export function computeVisibleDmChannels(
	dmChannels: Channel[],
	{
		expanded,
		showAll,
		activeChannel,
		visitedDms,
	}: { expanded: boolean; showAll: boolean; activeChannel: string | null; visitedDms: Set<string> },
): Channel[] {
	if (!expanded) return [];
	if (showAll) return dmChannels;
	return dmChannels.filter((ch) => ch.unread > 0 || ch.name === activeChannel || visitedDms.has(ch.name));
}

/**
 * Filter DM channels that should be displayed in the UI.
 * Root leads already have a real channel, so legacy `dm-<channel>` mirrors are hidden.
 */
export function getDisplayableDmChannels(channelList) {
	const regularChannelNames = new Set(
		channelList.filter((ch) => !(ch.is_dm || ch.name.startsWith("dm-"))).map((ch) => ch.name),
	);
	return channelList.filter((ch) => {
		if (!(ch.is_dm || ch.name.startsWith("dm-"))) return false;
		const dmPeer = ch.name.replace(/^dm-/, "");
		return !regularChannelNames.has(dmPeer);
	});
}

// ── Thread sidebar utilities ──────────────────────────────────────────────────

/**
 * Build a Set of threadParentIds that are already represented by a task.
 * A thread is "task-backed" when any task's thread_id or message_id matches it.
 */
export function getTaskThreadIds(kanban: KanbanData): Set<string> {
	const ids = new Set<string>();
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
export function getCompletedTaskThreadIds(kanban: KanbanData): Set<string> {
	const ids = new Set<string>();
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
 * Completed task threads are excluded (they appear in the needs-attention section instead).
 *
 * @param {string} channelName
 * @param {object} tracked - $trackedThreads store value
 * @param {object} unreadCounts - $threadUnreadCounts store value
 * @param {Set} taskThreadIds - from getTaskThreadIds() (active tasks)
 * @param {Set} completedTaskThreadIds - from getCompletedTaskThreadIds()
 * @returns {{ threads: Array<{id, subject, lastActivity, replyCount, unread}>, toClean: string[] }}
 */
export function getChannelThreads(
	channelName: string,
	tracked: Record<string, TrackedThread>,
	unreadCounts: Record<string, number>,
	taskThreadIds: Set<string>,
	completedTaskThreadIds: Set<string> = new Set(),
) {
	const threads: {
		id: string;
		subject: string;
		fullText: string;
		lastActivity: string;
		replyCount: number;
		unread: number;
	}[] = [];
	const toClean: string[] = [];
	for (const [id, entry] of Object.entries(tracked)) {
		if (entry.channelName !== channelName) continue;
		if (taskThreadIds.has(id)) {
			toClean.push(id);
			continue;
		}
		// Completed threads are shown in the needs-attention section, not per-channel
		if (completedTaskThreadIds.has(id)) continue;
		threads.push({
			id,
			subject: entry.subject,
			fullText: entry.fullText || entry.subject,
			lastActivity: entry.lastActivity,
			replyCount: entry.replyCount || 0,
			unread: unreadCounts[id] || 0,
		});
	}
	threads.sort((a, b) => (b.lastActivity || "").localeCompare(a.lastActivity || ""));
	return { threads, toClean };
}

/**
 * Get all completed threads across all channels, sorted by lastActivity (newest first).
 * These are threads whose parent task has been completed — they appear in the
 * "needs attention" sidebar section instead of their channel's thread list.
 *
 * @param {object} tracked - $trackedThreads store value
 * @param {object} unreadCounts - $threadUnreadCounts store value
 * @param {Set} taskThreadIds - from getTaskThreadIds() (active tasks)
 * @param {Set} completedTaskThreadIds - from getCompletedTaskThreadIds()
 * @returns {Array<{id, subject, fullText, lastActivity, replyCount, unread, channelName}>}
 */
export function getAllCompletedThreads(
	tracked: Record<string, TrackedThread>,
	unreadCounts: Record<string, number>,
	taskThreadIds: Set<string>,
	completedTaskThreadIds: Set<string>,
) {
	const threads: {
		id: string;
		subject: string;
		fullText: string;
		lastActivity: string;
		replyCount: number;
		unread: number;
		channelName: string;
	}[] = [];
	for (const [id, entry] of Object.entries(tracked)) {
		// Skip active task-backed threads
		if (taskThreadIds.has(id)) continue;
		// Only include completed threads
		if (!completedTaskThreadIds.has(id)) continue;
		threads.push({
			id,
			subject: entry.subject,
			fullText: entry.fullText || entry.subject,
			lastActivity: entry.lastActivity,
			replyCount: entry.replyCount || 0,
			unread: unreadCounts[id] || 0,
			channelName: entry.channelName,
		});
	}
	threads.sort((a, b) => (b.lastActivity || "").localeCompare(a.lastActivity || ""));
	return threads;
}

/**
 * Returns true if a channel has any tracked threads that aren't task-backed
 * and aren't completed (completed threads appear in needs-attention instead).
 */
export function getChannelHasTrackedThreads(
	channelName: string,
	tracked: Record<string, TrackedThread>,
	taskThreadIds: Set<string>,
	completedTaskThreadIds: Set<string> = new Set(),
): boolean {
	for (const [id, entry] of Object.entries(tracked)) {
		if (entry.channelName === channelName && !taskThreadIds.has(id) && !completedTaskThreadIds.has(id)) return true;
	}
	return false;
}

/**
 * Find a PR by number across kanban columns that contain PR data.
 * PRs appear in 'review' (open) and 'done' (merged) columns.
 */
export function findPr(
	prNum: string | number,
	kanbanData: KanbanData,
): PullRequest | MergedPullRequest | null | undefined {
	const num = parseInt(String(prNum), 10);
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
export function getPrUrl(
	prNum: string | number,
	kanbanData: KanbanData,
	repoStatuses: MultiRepoStatus[],
	primaryRepoFullName: string | null,
): string | null {
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
export function getChannelPrs(channelName: string, kanban: KanbanData): PullRequest[] {
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

// ── Tool block derivation utilities ───────────────────────────────────────────

/**
 * Collect all tool_data blocks from an array of channel messages.
 */
export function collectToolBlocks(messages: Message[]): ToolBlock[] {
	const blocks: ToolBlock[] = [];
	for (const msg of messages) {
		if (msg.tool_data?.length) {
			for (const block of msg.tool_data) {
				blocks.push(block);
			}
		}
	}
	return blocks;
}

/**
 * Determine whether any tool block is in-progress: output === null and no
 * later block with the same call_id has output set (completed).
 */
export function hasInProgressToolBlocks(allToolBlocks: ToolBlock[]): boolean {
	const completedCallIds = new Set();
	for (const block of allToolBlocks) {
		if (block.call_id && block.output != null) {
			completedCallIds.add(block.call_id);
		}
	}
	return allToolBlocks.some((block) => block.output == null && block.call_id && !completedCallIds.has(block.call_id));
}

/**
 * Find the most recent tool call entry for inline display.
 * Returns { toolName, callId, status } or null.
 */
export function getMostRecentToolCall(
	allToolBlocks: ToolBlock[],
): { toolName: string; callId: string | undefined; status: string } | null {
	if (allToolBlocks.length === 0) return null;
	const resultStatus: Record<string, string> = {};
	for (const block of allToolBlocks) {
		if (block.call_id && block.output != null) {
			resultStatus[block.call_id] = block.error ? "error" : "ok";
		}
	}
	for (let i = allToolBlocks.length - 1; i >= 0; i--) {
		const block = allToolBlocks[i];
		if (block.tool_name) {
			return {
				toolName: block.tool_name,
				callId: block.call_id,
				status: (block.call_id ? resultStatus[block.call_id] : undefined) || "InProgress",
			};
		}
	}
	return null;
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
export function resolveMessageTapAction({
	isWideScreen,
	msg,
	isInteractiveControl,
	link,
}: {
	isWideScreen: boolean;
	msg: Message;
	isInteractiveControl: boolean;
	link: { isExternal: boolean; dataset: { task?: string; pr?: string; channel?: string; coworker?: string } } | null;
}): { type: string; prNum?: string; taskId?: string } | null {
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
