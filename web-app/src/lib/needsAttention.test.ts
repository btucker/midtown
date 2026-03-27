import { describe, expect, it } from "vitest";
import { isTaskStale, threadNeedsAttention } from "./needsAttention.ts";

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
