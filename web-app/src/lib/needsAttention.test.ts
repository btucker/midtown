import { describe, expect, it } from "vitest";
import { computeAttentionItems, isTaskStale, threadNeedsAttention } from "./needsAttention.ts";

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
				{ id: 1, subject: "Fix bug", status: "completed", owner: "ghost-town", channel: "web" },
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
			tasks: [{ id: 1, subject: "Old completed", status: "completed", owner: "ghost-town" }],
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
