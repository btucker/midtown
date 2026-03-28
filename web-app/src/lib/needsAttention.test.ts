import { describe, expect, it } from "vitest";
import {
	computeAttentionItems,
	computeCompletedTaskItems,
	computeStaleTaskItems,
	computeThreadAttentionItems,
	isTaskStale,
	threadNeedsAttention,
} from "./needsAttention.ts";

describe("threadNeedsAttention", () => {
	const now = Date.now();
	const userSender = "human";

	it("returns false when last message is from user", () => {
		expect(
			threadNeedsAttention(
				{ sender: userSender, content: "hello", timestamp: new Date(now - 15 * 60000).toISOString() },
				userSender,
				now,
			),
		).toBe(false);
	});

	it("returns true when message is >10min old and not from user", () => {
		expect(
			threadNeedsAttention(
				{
					sender: "ghost-town",
					content: "done with the refactor",
					timestamp: new Date(now - 15 * 60000).toISOString(),
				},
				userSender,
				now,
			),
		).toBe(true);
	});

	it("returns true immediately when message @mentions user", () => {
		expect(
			threadNeedsAttention(
				{
					sender: "ghost-town",
					content: "hey @human what do you think?",
					timestamp: new Date(now - 1000).toISOString(),
				},
				userSender,
				now,
			),
		).toBe(true);
	});

	it("returns true immediately when message ends with ? and doesn't mention someone else", () => {
		expect(
			threadNeedsAttention(
				{
					sender: "ghost-town",
					content: "should I use Option or Result?",
					timestamp: new Date(now - 1000).toISOString(),
				},
				userSender,
				now,
			),
		).toBe(true);
	});

	it("returns false when message ends with ? but mentions someone else", () => {
		expect(
			threadNeedsAttention(
				{
					sender: "ghost-town",
					content: "@silver-fox should I use Option?",
					timestamp: new Date(now - 1000).toISOString(),
				},
				userSender,
				now,
			),
		).toBe(false);
	});

	it("returns false when message is <10min old and no special triggers", () => {
		expect(
			threadNeedsAttention(
				{ sender: "ghost-town", content: "working on it", timestamp: new Date(now - 5 * 60000).toISOString() },
				userSender,
				now,
			),
		).toBe(false);
	});
});

describe("isTaskStale", () => {
	const now = Date.now();

	it("returns false when progress changed recently", () => {
		expect(isTaskStale(50, now - 30 * 60000, now)).toBe(false);
	});

	it("returns true when no progress change for 2+ hours", () => {
		expect(isTaskStale(50, now - 3 * 3600000, now)).toBe(true);
	});

	it("returns false when task is done (progress 100)", () => {
		expect(isTaskStale(100, now - 3 * 3600000, now)).toBe(false);
	});
});

describe("computeAttentionItems", () => {
	const now = Date.now();
	const baseOpts = {
		trackedThreads: {},
		lastMessages: {},
		coworkers: [],
		tasks: [],
		progressTimestamps: {},
		threadReadState: {} as Record<string, string>,
		userSender: "human",
		mainChannel: "midtown",
		now,
	};

	it("returns completed tasks with status 'completed' (not 'done')", () => {
		const items = computeAttentionItems({
			...baseOpts,
			coworkers: [{ name: "ghost-town" }],
			tasks: [
				{
					id: 1,
					subject: "Fix bug",
					status: "completed",
					owner: "ghost-town",
					channel: "web",
					updated_at: new Date(now - 60000).toISOString(),
				},
				{ id: 2, subject: "Pending task", status: "pending", owner: "ghost-town" },
			],
		});
		expect(items).toHaveLength(1);
		expect(items[0].type).toBe("task_completed");
		expect(items[0].title).toBe("Fix bug");
	});

	it("filters out completed tasks older than 24 hours", () => {
		const items = computeAttentionItems({
			...baseOpts,
			coworkers: [{ name: "ghost-town" }],
			tasks: [
				{
					id: 1,
					subject: "Old completed",
					status: "completed",
					owner: "ghost-town",
					updated_at: new Date(now - 25 * 60 * 60 * 1000).toISOString(),
				},
				{
					id: 2,
					subject: "Recent completed",
					status: "completed",
					owner: "ghost-town",
					updated_at: new Date(now - 1 * 60 * 60 * 1000).toISOString(),
				},
			],
		});
		expect(items).toHaveLength(1);
		expect(items[0].title).toBe("Recent completed");
	});

	it("does NOT match tasks with status 'done'", () => {
		const items = computeAttentionItems({
			...baseOpts,
			tasks: [{ id: 1, subject: "Old task", status: "done", owner: "ghost-town" }],
		});
		expect(items).toHaveLength(0);
	});

	it("returns thread_waiting items when lastMessages is populated", () => {
		const threadId = "msg-123";
		const items = computeAttentionItems({
			...baseOpts,
			trackedThreads: {
				[threadId]: {
					channelName: "web",
					subject: "Auth discussion",
					lastActivity: new Date(now - 20 * 60000).toISOString(),
					replyCount: 3,
					lastReplySender: "ghost-town",
				},
			},
			lastMessages: {
				[threadId]: {
					sender: "ghost-town",
					content: "what do you think?",
					timestamp: new Date(now - 20 * 60000).toISOString(),
				},
			},
		});
		expect(items).toHaveLength(1);
		expect(items[0].type).toBe("thread_waiting");
		expect(items[0].threadId).toBe(threadId);
	});

	it("returns mention items when user is @mentioned", () => {
		const threadId = "msg-456";
		const items = computeAttentionItems({
			...baseOpts,
			trackedThreads: {
				[threadId]: {
					channelName: "web",
					subject: "Review needed",
					lastActivity: new Date(now - 1000).toISOString(),
					replyCount: 1,
				},
			},
			lastMessages: {
				[threadId]: {
					sender: "ghost-town",
					content: "hey @human please review",
					timestamp: new Date(now - 1000).toISOString(),
				},
			},
		});
		expect(items).toHaveLength(1);
		expect(items[0].type).toBe("mention");
	});

	it("returns stale_work items when progress hasn't changed for 2+ hours", () => {
		const items = computeAttentionItems({
			...baseOpts,
			tasks: [{ id: 10, subject: "Refactor auth", status: "in_progress", owner: "silver-fox", channel: "web" }],
			coworkers: [{ name: "silver-fox", progress: 30 }],
			progressTimestamps: { "10": now - 3 * 3600000 },
		});
		expect(items).toHaveLength(1);
		expect(items[0].type).toBe("stale_work");
		expect(items[0].title).toBe("Refactor auth");
	});

	it("skips thread when threadReadState shows it has been read", () => {
		const threadId = "msg-read-1";
		const msgTimestamp = new Date(now - 20 * 60000).toISOString();
		const readTimestamp = new Date(now - 10 * 60000).toISOString(); // read after last message
		const items = computeAttentionItems({
			...baseOpts,
			trackedThreads: {
				[threadId]: {
					channelName: "web",
					subject: "Already read",
					lastActivity: msgTimestamp,
					replyCount: 1,
				},
			},
			lastMessages: {
				[threadId]: {
					sender: "ghost-town",
					content: "what do you think?",
					timestamp: msgTimestamp,
				},
			},
			threadReadState: { [threadId]: readTimestamp },
		});
		expect(items).toHaveLength(0);
	});

	it("includes thread when threadReadState is older than last message", () => {
		const threadId = "msg-unread-1";
		const readTimestamp = new Date(now - 30 * 60000).toISOString(); // read before last message
		const msgTimestamp = new Date(now - 20 * 60000).toISOString(); // new message after read
		const items = computeAttentionItems({
			...baseOpts,
			trackedThreads: {
				[threadId]: {
					channelName: "web",
					subject: "New reply",
					lastActivity: msgTimestamp,
					replyCount: 2,
				},
			},
			lastMessages: {
				[threadId]: {
					sender: "ghost-town",
					content: "updated thoughts",
					timestamp: msgTimestamp,
				},
			},
			threadReadState: { [threadId]: readTimestamp },
		});
		expect(items).toHaveLength(1);
		expect(items[0].type).toBe("thread_waiting");
	});

	it("sorts items newest first", () => {
		const threadId = "msg-789";
		const items = computeAttentionItems({
			...baseOpts,
			coworkers: [{ name: "ghost-town" }],
			tasks: [
				{
					id: 1,
					subject: "Old completed",
					status: "completed",
					owner: "ghost-town",
					updated_at: new Date(now - 60000).toISOString(),
				},
			],
			trackedThreads: {
				[threadId]: {
					channelName: "web",
					subject: "Recent thread",
					lastActivity: new Date(now - 15 * 60000).toISOString(),
					replyCount: 1,
				},
			},
			lastMessages: {
				[threadId]: { sender: "ghost-town", content: "update?", timestamp: new Date(now - 15 * 60000).toISOString() },
			},
		});
		expect(items).toHaveLength(2);
		// task_completed uses `now` as timestamp, thread uses the message timestamp (15min ago)
		expect(items[0].type).toBe("task_completed");
		expect(items[1].type).toBe("thread_waiting");
	});
});

// ── Sub-function tests (independent derived branches) ───────────────────────

describe("computeThreadAttentionItems", () => {
	const now = Date.now();

	it("returns thread_waiting items for unread threads with old messages", () => {
		const threadId = "msg-100";
		const items = computeThreadAttentionItems({
			trackedThreads: {
				[threadId]: {
					channelName: "web",
					subject: "Auth discussion",
					lastActivity: new Date(now - 20 * 60000).toISOString(),
					replyCount: 3,
					lastReplySender: "ghost-town",
				},
			},
			lastMessages: {
				[threadId]: {
					sender: "ghost-town",
					content: "what do you think?",
					timestamp: new Date(now - 20 * 60000).toISOString(),
				},
			},
			threadReadState: {},
			userSender: "human",
			now,
		});
		expect(items).toHaveLength(1);
		expect(items[0].type).toBe("thread_waiting");
		expect(items[0].threadId).toBe(threadId);
	});

	it("returns mention items when user is @mentioned", () => {
		const threadId = "msg-101";
		const items = computeThreadAttentionItems({
			trackedThreads: {
				[threadId]: {
					channelName: "web",
					subject: "Review needed",
					lastActivity: new Date(now - 1000).toISOString(),
					replyCount: 1,
				},
			},
			lastMessages: {
				[threadId]: {
					sender: "ghost-town",
					content: "hey @human please review",
					timestamp: new Date(now - 1000).toISOString(),
				},
			},
			threadReadState: {},
			userSender: "human",
			now,
		});
		expect(items).toHaveLength(1);
		expect(items[0].type).toBe("mention");
	});

	it("skips threads already read", () => {
		const threadId = "msg-102";
		const msgTimestamp = new Date(now - 20 * 60000).toISOString();
		const items = computeThreadAttentionItems({
			trackedThreads: {
				[threadId]: {
					channelName: "web",
					subject: "Already read",
					lastActivity: msgTimestamp,
					replyCount: 1,
				},
			},
			lastMessages: {
				[threadId]: { sender: "ghost-town", content: "update?", timestamp: msgTimestamp },
			},
			threadReadState: { [threadId]: new Date(now - 10 * 60000).toISOString() },
			userSender: "human",
			now,
		});
		expect(items).toHaveLength(0);
	});

	it("does not depend on tasks or coworkers", () => {
		// Calling with only thread data — no task/coworker fields needed
		const items = computeThreadAttentionItems({
			trackedThreads: {},
			lastMessages: {},
			threadReadState: {},
			userSender: "human",
			now,
		});
		expect(items).toHaveLength(0);
	});
});

describe("computeCompletedTaskItems", () => {
	const now = Date.now();
	const cwMap = new Map([["ghost-town", { name: "ghost-town", pr_number: 42 }]]);

	it("returns completed tasks within 24h that are unread", () => {
		const items = computeCompletedTaskItems({
			tasks: [
				{
					id: 1,
					subject: "Fix bug",
					status: "completed",
					owner: "ghost-town",
					channel: "web",
					updated_at: new Date(now - 60000).toISOString(),
				},
			],
			coworkerMap: cwMap,
			threadReadState: {},
			mainChannel: "midtown",
			now,
		});
		expect(items).toHaveLength(1);
		expect(items[0].type).toBe("task_completed");
		expect(items[0].context).toContain("PR #42");
	});

	it("filters out tasks older than 24h", () => {
		const items = computeCompletedTaskItems({
			tasks: [
				{
					id: 1,
					subject: "Old task",
					status: "completed",
					owner: "ghost-town",
					updated_at: new Date(now - 25 * 3600000).toISOString(),
				},
			],
			coworkerMap: cwMap,
			threadReadState: {},
			mainChannel: "midtown",
			now,
		});
		expect(items).toHaveLength(0);
	});

	it("filters out already-read completed tasks", () => {
		const updatedAt = new Date(now - 60000).toISOString();
		const items = computeCompletedTaskItems({
			tasks: [
				{
					id: 1,
					subject: "Read task",
					status: "completed",
					owner: "ghost-town",
					updated_at: updatedAt,
				},
			],
			coworkerMap: cwMap,
			threadReadState: { "task:1": new Date(now).toISOString() },
			mainChannel: "midtown",
			now,
		});
		expect(items).toHaveLength(0);
	});

	it("skips non-completed tasks", () => {
		const items = computeCompletedTaskItems({
			tasks: [{ id: 1, subject: "In progress", status: "in_progress", owner: "ghost-town" }],
			coworkerMap: cwMap,
			threadReadState: {},
			mainChannel: "midtown",
			now,
		});
		expect(items).toHaveLength(0);
	});
});

describe("computeStaleTaskItems", () => {
	const now = Date.now();
	const cwMap = new Map([["silver-fox", { name: "silver-fox", progress: 30 }]]);

	it("returns stale in-progress tasks", () => {
		const items = computeStaleTaskItems({
			tasks: [{ id: 10, subject: "Refactor auth", status: "in_progress", owner: "silver-fox", channel: "web" }],
			coworkerMap: cwMap,
			progressTimestamps: { "10": now - 3 * 3600000 },
			mainChannel: "midtown",
			now,
		});
		expect(items).toHaveLength(1);
		expect(items[0].type).toBe("stale_work");
		expect(items[0].context).toContain("30%");
	});

	it("skips tasks with recent progress", () => {
		const items = computeStaleTaskItems({
			tasks: [{ id: 10, subject: "Active work", status: "in_progress", owner: "silver-fox" }],
			coworkerMap: cwMap,
			progressTimestamps: { "10": now - 30 * 60000 },
			mainChannel: "midtown",
			now,
		});
		expect(items).toHaveLength(0);
	});

	it("skips tasks without progress timestamps", () => {
		const items = computeStaleTaskItems({
			tasks: [{ id: 10, subject: "New task", status: "in_progress", owner: "silver-fox" }],
			coworkerMap: cwMap,
			progressTimestamps: {},
			mainChannel: "midtown",
			now,
		});
		expect(items).toHaveLength(0);
	});

	it("skips completed tasks", () => {
		const items = computeStaleTaskItems({
			tasks: [{ id: 10, subject: "Done", status: "completed", owner: "silver-fox" }],
			coworkerMap: cwMap,
			progressTimestamps: { "10": now - 3 * 3600000 },
			mainChannel: "midtown",
			now,
		});
		expect(items).toHaveLength(0);
	});
});
