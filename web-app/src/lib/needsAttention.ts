import { getSenderColor } from "./messageUtils.ts";
import type { Coworker, NeedsAttentionItem, Task, TrackedThread } from "./types.ts";

const TEN_MINUTES_MS = 10 * 60 * 1000;
const TWO_HOURS_MS = 2 * 60 * 60 * 1000;
const TWENTY_FOUR_HOURS_MS = 24 * 60 * 60 * 1000;

export interface LastMessage {
	sender: string;
	content: string;
	timestamp: string; // ISO
}

/**
 * Determine if a thread needs the user's attention based on the last message.
 *
 * Returns true when:
 * 1. Last message is NOT from the user, AND
 * 2. At least one of:
 *    - Message is >10 minutes old
 *    - Message @mentions the user
 *    - Message ends with ? and doesn't @mention someone else
 */
export function threadNeedsAttention(lastMsg: LastMessage, userSender: string, now: number = Date.now()): boolean {
	if (lastMsg.sender === userSender) return false;

	const ageMs = now - new Date(lastMsg.timestamp).getTime();
	const content = lastMsg.content.trim();

	// Immediate: @mentions user
	if (content.includes(`@${userSender}`)) return true;

	// Immediate: ends with ? and doesn't @mention someone else
	if (content.endsWith("?")) {
		const mentionPattern = /@[\w-]+/g;
		const mentions = content.match(mentionPattern) || [];
		const mentionsOther = mentions.some((m) => m !== `@${userSender}`);
		if (!mentionsOther) return true;
	}

	// Delayed: >10 minutes old
	if (ageMs > TEN_MINUTES_MS) return true;

	return false;
}

/**
 * Determine if a task is stale (no progress change for 2+ hours).
 */
export function isTaskStale(
	progress: number | null | undefined,
	lastProgressChangeMs: number,
	now: number = Date.now(),
): boolean {
	if (progress === 100 || progress == null) return false;
	return now - lastProgressChangeMs > TWO_HOURS_MS;
}

/**
 * Build the full list of needs-attention items from current state.
 */
export function computeAttentionItems(opts: {
	trackedThreads: Record<string, TrackedThread>;
	lastMessages: Record<string, LastMessage>;
	coworkers: Coworker[];
	tasks: Task[];
	progressTimestamps: Record<string, number>;
	threadReadState: Record<string, string>;
	userSender: string;
	mainChannel: string;
	now?: number;
}): NeedsAttentionItem[] {
	const now = opts.now ?? Date.now();
	const items: NeedsAttentionItem[] = [];

	// 1. Threads needing attention
	for (const [threadId, tracked] of Object.entries(opts.trackedThreads)) {
		const lastMsg = opts.lastMessages[threadId];
		if (!lastMsg) continue;

		// Skip if thread is read (user has seen it since last message)
		const lastRead = opts.threadReadState[threadId];
		if (lastRead && new Date(lastRead) >= new Date(lastMsg.timestamp)) continue;

		if (threadNeedsAttention(lastMsg, opts.userSender, now)) {
			const ageMs = now - new Date(lastMsg.timestamp).getTime();
			const agoText = formatAgo(ageMs);

			items.push({
				id: `thread:${threadId}`,
				type: lastMsg.content.includes(`@${opts.userSender}`) ? "mention" : "thread_waiting",
				title: tracked.subject,
				context: `${lastMsg.sender} replied ${agoText} · waiting on you`,
				channel: tracked.channelName,
				threadId,
				timestamp: new Date(lastMsg.timestamp).getTime(),
				workerName: lastMsg.sender,
				workerColor: getSenderColor(lastMsg.sender, null),
			});
		}
	}

	// 2. Completed tasks — only show if completed within the last 24 hours and not yet seen
	for (const task of opts.tasks) {
		if (task.status !== "completed") continue;

		// Filter out old completions — only show tasks updated in the last 24h
		const updatedMs = task.updated_at ? new Date(task.updated_at).getTime() : 0;
		if (now - updatedMs > TWENTY_FOUR_HOURS_MS) continue;

		// Filter out if user has already seen this completed task
		const taskReadKey = `task:${task.id}`;
		const lastRead = opts.threadReadState[taskReadKey];
		if (lastRead && new Date(lastRead).getTime() >= updatedMs) continue;

		const id = taskReadKey;
		const cw = opts.coworkers.find((c) => c.name === task.owner);
		const channel = task.channel || opts.mainChannel;

		items.push({
			id,
			type: "task_completed",
			title: task.subject,
			context: `Task completed by ${task.owner || "unknown"}${cw?.pr_number ? ` · PR #${cw.pr_number} ready` : ""} · #${channel}`,
			channel,
			taskId: task.id,
			timestamp: now,
			workerName: task.owner,
			workerColor: task.color || (task.owner ? getSenderColor(task.owner, null) : undefined),
		});
	}

	// 3. Stale tasks
	for (const task of opts.tasks) {
		if (task.status !== "in_progress") continue;
		const cw = opts.coworkers.find((c) => c.name === task.owner);
		const lastChange = opts.progressTimestamps[String(task.id)];
		if (!lastChange) continue;

		if (isTaskStale(cw?.progress ?? null, lastChange, now)) {
			const id = `stale:${task.id}`;

			const channel = task.channel || opts.mainChannel;
			const staleHours = Math.floor((now - lastChange) / 3600000);

			items.push({
				id,
				type: "stale_work",
				title: task.subject,
				context: `No progress from ${task.owner || "unknown"} for ${staleHours}h · ${cw?.progress ?? 0}% complete · #${channel}`,
				channel,
				taskId: task.id,
				timestamp: lastChange,
				workerName: task.owner,
				workerColor: task.color || (task.owner ? getSenderColor(task.owner, null) : undefined),
			});
		}
	}

	// Sort newest first
	items.sort((a, b) => b.timestamp - a.timestamp);
	return items;
}

function formatAgo(ms: number): string {
	const minutes = Math.floor(ms / 60000);
	if (minutes < 1) return "just now";
	if (minutes < 60) return `${minutes}m ago`;
	const hours = Math.floor(minutes / 60);
	return `${hours}h ago`;
}
