import { describe, expect, it } from "vitest";
import {
	collectToolBlocks,
	computeExpandedAfterChannelNameClick,
	computeExpandedAfterTriangleClick,
	computeVisibleDmChannels,
	findPr,
	getAllCompletedThreads,
	getChannelCiStatus,
	getChannelHasTrackedThreads,
	getChannelPrs,
	getChannelTaskCount,
	getChannelThreads,
	getDisplayableDmChannels,
	getMostRecentToolCall,
	getPrUrl,
	hasInProgressToolBlocks,
	resolveMessageTapAction,
} from "./channelUtils.ts";
import type { KanbanData, Message, ToolBlock, TrackedThread } from "./types.ts";

describe("getChannelTaskCount", () => {
	const mockKanban = {
		// Tasks use explicit `channel` field matching the backend structure
		inProgress: [
			{ id: 101, title: "Add JWT", channel: "auth-refactor" },
			{ id: 102, title: "Dark mode", channel: "ui-improvements" },
			{ id: 103, title: "Other task", channel: null }, // No explicit channel = midtown default
		],
		backlog: [
			{ id: 201, title: "Update tests", channel: "auth-refactor" },
			{ id: 202, title: "Unrelated pending task", channel: null },
		],
		// PRs reference tasks via task_id — channel resolved via task lookup
		review: [{ task_id: 101, task_name: "Add JWT" }],
	} as unknown as KanbanData;

	it("returns only midtown-owned tasks for midtown channel", () => {
		// mockKanban has 3 inProgress: channel='auth-refactor', 'ui-improvements', null
		// and 2 pending: channel='auth-refactor', null
		// midtown should only see tasks with no channel (or channel==='midtown')
		const counts = getChannelTaskCount("midtown", mockKanban);
		expect(counts).toEqual({
			inProgress: 1, // only the channel:null task
			pending: 1, // only the channel:null task
			review: 0, // PR's task is in 'auth-refactor', not midtown
		});
	});

	it("does not show tasks assigned to other channels in midtown", () => {
		// Regression: tasks with an explicit channel field were appearing in both
		// midtown AND their assigned channel, causing duplicates in the sidebar.
		const kanban = {
			inProgress: [
				{ id: 1778, title: "Auth profile pool", channel: "multi-platform" },
				{ id: 1779, title: "Auth profile pool: integrati...", channel: "multi-platform" },
				{ id: 1780, title: "Midtown task", channel: null },
			],
			backlog: [
				{ id: 1781, title: "Another multi-platform task", channel: "multi-platform" },
				{ id: 1782, title: "Unassigned task", channel: null },
			],
			review: [],
		} as unknown as KanbanData;
		const counts = getChannelTaskCount("midtown", kanban);
		// Only tasks with no channel (or channel==='midtown') should appear
		expect(counts.inProgress).toBe(1);
		expect(counts.pending).toBe(1);
	});

	it("filters tasks by channel field for topic channels", () => {
		const counts = getChannelTaskCount("auth-refactor", mockKanban);
		expect(counts).toEqual({
			inProgress: 1,
			pending: 1,
			review: 1,
		});
	});

	it("returns zero counts for channel with no matching tasks", () => {
		const counts = getChannelTaskCount("nonexistent", mockKanban);
		expect(counts).toEqual({
			inProgress: 0,
			pending: 0,
			review: 0,
		});
	});

	it("groups PRs by task channel, not task_name text", () => {
		// PR's task_name does NOT contain the channel name,
		// but the corresponding task has the correct channel field
		const kanban = {
			inProgress: [{ id: 50, title: "Implement feature", channel: "my-channel" }],
			backlog: [],
			review: [{ task_id: 50, task_name: "Implement feature" }],
		} as unknown as KanbanData;
		const counts = getChannelTaskCount("my-channel", kanban);
		expect(counts.review).toBe(1);
	});

	it("PRs without task_id only appear in main channel", () => {
		const kanban = {
			inProgress: [{ id: 50, title: "Task", channel: "my-channel" }],
			backlog: [],
			review: [
				{ task_name: "Orphan PR" }, // no task_id
			],
		} as unknown as KanbanData;
		const mainCounts = getChannelTaskCount("midtown", kanban);
		const topicCounts = getChannelTaskCount("my-channel", kanban);
		expect(mainCounts.review).toBe(1);
		expect(topicCounts.review).toBe(0);
	});
});

describe("getChannelCiStatus", () => {
	it("returns null when no PRs match", () => {
		const mockKanban = { inProgress: [], backlog: [], review: [] } as unknown as KanbanData;
		expect(getChannelCiStatus("auth", mockKanban)).toBe(null);
	});

	it('returns "failed" if any PR has ci_failed', () => {
		const mockKanban = {
			inProgress: [
				{ id: 1, title: "Task 1", channel: "auth" },
				{ id: 2, title: "Task 2", channel: "auth" },
			],
			backlog: [],
			review: [
				{ task_id: 1, task_name: "Task 1", status: "ci_passed" },
				{ task_id: 2, task_name: "Task 2", status: "ci_failed" },
			],
		} as unknown as KanbanData;
		expect(getChannelCiStatus("auth", mockKanban)).toBe("failed");
	});

	it('returns "pending" if any PR has ci_pending and none failed', () => {
		const mockKanban = {
			inProgress: [
				{ id: 1, title: "Task 1", channel: "auth" },
				{ id: 2, title: "Task 2", channel: "auth" },
			],
			backlog: [],
			review: [
				{ task_id: 1, task_name: "Task 1", status: "ci_passed" },
				{ task_id: 2, task_name: "Task 2", status: "ci_pending" },
			],
		} as unknown as KanbanData;
		expect(getChannelCiStatus("auth", mockKanban)).toBe("pending");
	});

	it('returns "passed" if all PRs passed or approved', () => {
		const mockKanban = {
			inProgress: [
				{ id: 1, title: "Task 1", channel: "auth" },
				{ id: 2, title: "Task 2", channel: "auth" },
			],
			backlog: [],
			review: [
				{ task_id: 1, task_name: "Task 1", status: "ci_passed" },
				{ task_id: 2, task_name: "Task 2", status: "approved" },
			],
		} as unknown as KanbanData;
		expect(getChannelCiStatus("auth", mockKanban)).toBe("passed");
	});

	it("returns status for all PRs in midtown channel", () => {
		const mockKanban = {
			inProgress: [],
			backlog: [],
			review: [{ task_id: 1, task_name: "Task 1", status: "ci_failed" }],
		} as unknown as KanbanData;
		expect(getChannelCiStatus("midtown", mockKanban)).toBe("failed");
	});
});

describe("getChannelPrs", () => {
	const mockKanban = {
		inProgress: [
			{ id: 101, title: "Add JWT", channel: "auth-refactor" },
			{ id: 102, title: "Dark mode", channel: "ui-improvements" },
		],
		backlog: [],
		review: [
			{ task_id: 101, task_name: "Add JWT", number: 42 },
			{ task_id: 102, task_name: "Dark mode", number: 43 },
			{ task_name: "Other PR", number: 44 }, // no task_id
		],
	} as unknown as KanbanData;

	it("returns only midtown-owned PRs for midtown channel", () => {
		// PR #44 has no task_id → goes to midtown
		// PR #42 has task_id:101 (channel:'auth-refactor') → not midtown
		// PR #43 has task_id:102 (channel:'ui-improvements') → not midtown
		const prs = getChannelPrs("midtown", mockKanban);
		expect(prs).toHaveLength(1);
		expect(prs[0].number).toBe(44);
	});

	it("filters PRs by task channel for topic channels", () => {
		const prs = getChannelPrs("auth-refactor", mockKanban);
		expect(prs).toHaveLength(1);
		expect(prs[0].number).toBe(42);
	});

	it("returns empty array for channel with no matching PRs", () => {
		const prs = getChannelPrs("nonexistent", mockKanban);
		expect(prs).toEqual([]);
	});

	it("groups PRs by task channel, not task_name text", () => {
		// PR task_name does NOT mention the channel name
		const kanban = {
			inProgress: [{ id: 50, title: "Implement feature", channel: "special-channel" }],
			backlog: [],
			review: [{ task_id: 50, task_name: "Implement feature", number: 99 }],
		} as unknown as KanbanData;
		const prs = getChannelPrs("special-channel", kanban);
		expect(prs).toHaveLength(1);
		expect(prs[0].number).toBe(99);
	});
});

describe("getChannelThreads", () => {
	const tracked: Record<string, TrackedThread> = {
		"thread-1": { channelName: "web", subject: "Thread 1", lastActivity: "2026-03-04T10:00:00Z", replyCount: 3 },
		"thread-2": { channelName: "web", subject: "Thread 2", lastActivity: "2026-03-04T11:00:00Z", replyCount: 1 },
		"thread-3": { channelName: "auth", subject: "Auth thread", lastActivity: "2026-03-04T09:00:00Z", replyCount: 0 },
		"task-thread": {
			channelName: "web",
			subject: "Task-backed thread",
			lastActivity: "2026-03-04T12:00:00Z",
			replyCount: 0,
		},
	};
	const unreadCounts = { "thread-1": 2, "thread-2": 0, "task-thread": 5 };
	const taskThreadIds = new Set(["task-thread"]);

	it("returns threads for the given channel, filtered and sorted by lastActivity", () => {
		const result = getChannelThreads("web", tracked, unreadCounts, taskThreadIds);
		expect(result.threads).toHaveLength(2);
		// sorted newest first: thread-2 (11:00) before thread-1 (10:00)
		expect(result.threads[0].id).toBe("thread-2");
		expect(result.threads[1].id).toBe("thread-1");
	});

	it("includes unread counts in thread objects", () => {
		const result = getChannelThreads("web", tracked, unreadCounts, taskThreadIds);
		const t1 = result.threads.find((t) => t.id === "thread-1");
		const t2 = result.threads.find((t) => t.id === "thread-2");
		expect(t1?.unread).toBe(2);
		expect(t2?.unread).toBe(0);
	});

	it("excludes task-backed threads from the thread list", () => {
		const result = getChannelThreads("web", tracked, unreadCounts, taskThreadIds);
		const ids = result.threads.map((t) => t.id);
		expect(ids).not.toContain("task-thread");
	});

	it("returns toClean IDs for task-backed threads (no side-effect callback)", () => {
		const result = getChannelThreads("web", tracked, unreadCounts, taskThreadIds);
		expect(result.toClean).toEqual(["task-thread"]);
	});

	it("returns empty toClean when no task-backed threads exist", () => {
		const result = getChannelThreads("web", tracked, unreadCounts, new Set());
		expect(result.toClean).toEqual([]);
	});

	it("filters by channel name", () => {
		const result = getChannelThreads("auth", tracked, unreadCounts, taskThreadIds);
		expect(result.threads).toHaveLength(1);
		expect(result.threads[0].id).toBe("thread-3");
	});
});

describe("computeExpandedAfterTriangleClick", () => {
	it("expands a collapsed channel", () => {
		const result = computeExpandedAfterTriangleClick("web", new Set());
		expect(result.has("web")).toBe(true);
	});

	it("collapses an expanded channel", () => {
		const result = computeExpandedAfterTriangleClick("web", new Set(["web"]));
		expect(result.has("web")).toBe(false);
	});

	it("does not affect other channels", () => {
		const result = computeExpandedAfterTriangleClick("web", new Set(["auth", "web"]));
		expect(result.has("auth")).toBe(true);
		expect(result.has("web")).toBe(false);
	});

	it("returns a new set (does not mutate the original)", () => {
		const original = new Set(["web"]);
		const result = computeExpandedAfterTriangleClick("web", original);
		expect(original.has("web")).toBe(true);
		expect(result.has("web")).toBe(false);
	});
});

describe("computeExpandedAfterChannelNameClick", () => {
	const mockKanban = {
		inProgress: [{ id: 1, title: "Build feature", channel: "web" }],
		backlog: [],
		review: [],
	} as unknown as KanbanData;

	it("auto-expands when switching to inactive channel with active tasks", () => {
		const result = computeExpandedAfterChannelNameClick("web", new Set(), "midtown", mockKanban);
		expect(result.has("web")).toBe(true);
	});

	it("does not expand when switching to inactive channel without tasks", () => {
		const result = computeExpandedAfterChannelNameClick("empty", new Set(), "midtown", mockKanban);
		expect(result.has("empty")).toBe(false);
	});

	it("expands already-active collapsed channel (toggle)", () => {
		const result = computeExpandedAfterChannelNameClick("web", new Set(), "web", mockKanban);
		expect(result.has("web")).toBe(true);
	});

	it("collapses already-active expanded channel (toggle)", () => {
		const result = computeExpandedAfterChannelNameClick("web", new Set(["web"]), "web", mockKanban);
		expect(result.has("web")).toBe(false);
	});

	it("keeps expanded state when switching to already-expanded inactive channel", () => {
		const result = computeExpandedAfterChannelNameClick("web", new Set(["web"]), "midtown", mockKanban);
		expect(result.has("web")).toBe(true);
	});

	it("does not collapse other expanded channels when switching", () => {
		const result = computeExpandedAfterChannelNameClick("web", new Set(["auth"]), "midtown", mockKanban);
		expect(result.has("auth")).toBe(true);
		expect(result.has("web")).toBe(true);
	});
});

describe("computeVisibleDmChannels", () => {
	const dmChannels = [
		{ name: "dm-alice", unread: 2, is_dm: true },
		{ name: "dm-bob", unread: 0, is_dm: true },
		{ name: "dm-carol", unread: 0, is_dm: true },
	];

	it("returns empty array when section is collapsed", () => {
		const result = computeVisibleDmChannels(dmChannels, {
			expanded: false,
			showAll: false,
			activeChannel: "dm-alice",
			visitedDms: new Set(),
		});
		expect(result).toEqual([]);
	});

	it("returns all DMs when showAll is true", () => {
		const result = computeVisibleDmChannels(dmChannels, {
			expanded: true,
			showAll: true,
			activeChannel: "midtown",
			visitedDms: new Set(),
		});
		expect(result).toEqual(dmChannels);
	});

	it("shows unread DMs when expanded", () => {
		const result = computeVisibleDmChannels(dmChannels, {
			expanded: true,
			showAll: false,
			activeChannel: "midtown",
			visitedDms: new Set(),
		});
		expect(result.map((ch) => ch.name)).toEqual(["dm-alice"]);
	});

	it("shows the active DM even if it has no unread messages", () => {
		const result = computeVisibleDmChannels(dmChannels, {
			expanded: true,
			showAll: false,
			activeChannel: "dm-bob",
			visitedDms: new Set(),
		});
		expect(result.map((ch) => ch.name)).toContain("dm-bob");
	});

	it("keeps a visited DM visible after navigating away (Bug #2 regression)", () => {
		// Scenario: user opened dm-bob (visited), then switched to a regular channel.
		// dm-bob has unread=0 and is not activeChannel — but it was visited, so it
		// should remain visible.
		const result = computeVisibleDmChannels(dmChannels, {
			expanded: true,
			showAll: false,
			activeChannel: "midtown",
			visitedDms: new Set(["dm-bob"]),
		});
		expect(result.map((ch) => ch.name)).toContain("dm-bob");
	});

	it("shows unread + active + visited DMs together", () => {
		const result = computeVisibleDmChannels(dmChannels, {
			expanded: true,
			showAll: false,
			activeChannel: "dm-carol",
			visitedDms: new Set(["dm-bob"]),
		});
		// dm-alice: unread > 0, dm-bob: visited, dm-carol: active
		expect(result.map((ch) => ch.name)).toEqual(["dm-alice", "dm-bob", "dm-carol"]);
	});

	it('"show less" is redundant when all DMs are visited (no hidden channels)', () => {
		// Scenario: 3 DMs, all visited, none unread. Clicking "show all" then
		// "show less" should return to the same set — so "show less" should not
		// appear. We verify by checking that the filtered count (showAll=false)
		// equals the total count, making the guard `total > filtered` false.
		const allVisited = new Set(["dm-alice", "dm-bob", "dm-carol"]);
		const allDmsNoUnread = [
			{ name: "dm-alice", unread: 0, is_dm: true },
			{ name: "dm-bob", unread: 0, is_dm: true },
			{ name: "dm-carol", unread: 0, is_dm: true },
		];
		const filtered = computeVisibleDmChannels(allDmsNoUnread, {
			expanded: true,
			showAll: false,
			activeChannel: "midtown",
			visitedDms: allVisited,
		});
		// All 3 are visited, so filtered set = full set → "show less" is redundant
		expect(filtered.length).toBe(allDmsNoUnread.length);
	});
});

describe("getDisplayableDmChannels", () => {
	it("hides legacy DM mirrors for root leads that already own a real channel", () => {
		const result = getDisplayableDmChannels([
			{ name: "midtown", is_dm: false },
			{ name: "auth", is_dm: false },
			{ name: "dm-auth", is_dm: true },
			{ name: "dm-midtown", is_dm: true },
			{ name: "dm-park", is_dm: true },
		]);

		expect(result.map((ch) => ch.name)).toEqual(["dm-park"]);
	});

	it("hides DM mirrors for fork sessions that stream to their bound thread", () => {
		const forkNames = new Set(["auth-discuss-a1b2"]);
		const result = getDisplayableDmChannels(
			[
				{ name: "midtown", is_dm: false },
				{ name: "dm-park", is_dm: true },
				{ name: "dm-auth-discuss-a1b2", is_dm: true },
			],
			forkNames,
		);

		expect(result.map((ch) => ch.name)).toEqual(["dm-park"]);
	});
});

// ── PR link navigation ──────────────────────────────────────────────────────
// PR #N links must always open GitHub, never redirect to a task thread.
// Both desktop (handleLinkClick) and mobile (handleMessageTap) use getPrUrl
// to resolve the destination. These tests verify getPrUrl always returns a
// GitHub URL regardless of task association.

describe("findPr", () => {
	const kanban = {
		review: [{ number: 42, task_id: 7, repo: "main" }],
		done: [{ number: 10, task_id: null }],
	} as unknown as KanbanData;

	it("finds a PR in the review column", () => {
		expect(findPr(42, kanban)).toEqual({ number: 42, task_id: 7, repo: "main" });
	});

	it("finds a PR in the done column", () => {
		expect(findPr(10, kanban)).toEqual({ number: 10, task_id: null });
	});

	it("returns null for unknown PR number", () => {
		expect(findPr(999, kanban)).toBeNull();
	});

	it("parses string PR numbers", () => {
		expect(findPr("42", kanban)).toEqual({ number: 42, task_id: 7, repo: "main" });
	});
});

describe("getPrUrl", () => {
	const primaryRepo = "btucker/midtown";

	it("returns GitHub URL for PR with associated task", () => {
		// Key invariant: even when a PR has a task_id, we get a GitHub URL, not a task thread
		const kanban = {
			review: [{ number: 42, task_id: 7, repo: null }],
			done: [],
		} as unknown as KanbanData;
		const url = getPrUrl(42, kanban, [], primaryRepo);
		expect(url).toBe("https://github.com/btucker/midtown/pull/42");
	});

	it("returns GitHub URL for PR without associated task", () => {
		const kanban = {
			review: [{ number: 10, task_id: null, repo: null }],
			done: [],
		} as unknown as KanbanData;
		const url = getPrUrl(10, kanban, [], primaryRepo);
		expect(url).toBe("https://github.com/btucker/midtown/pull/10");
	});

	it("resolves multi-repo PR via repoStatuses", () => {
		const kanban = {
			review: [{ number: 5, task_id: 3, repo: "docs" }],
			done: [],
		} as unknown as KanbanData;
		const repoStatuses = [{ label: "docs", fullName: "btucker/midtown-docs" }];
		const url = getPrUrl(5, kanban, repoStatuses, primaryRepo);
		expect(url).toBe("https://github.com/btucker/midtown-docs/pull/5");
	});

	it("falls back to primary repo when multi-repo label has no match", () => {
		const kanban = {
			review: [{ number: 5, task_id: null, repo: "unknown-label" }],
			done: [],
		} as unknown as KanbanData;
		const url = getPrUrl(5, kanban, [], primaryRepo);
		expect(url).toBe("https://github.com/btucker/midtown/pull/5");
	});

	it("falls back to primary repo when PR is not in kanban", () => {
		const kanban = { review: [], done: [] } as unknown as KanbanData;
		const url = getPrUrl(99, kanban, [], primaryRepo);
		expect(url).toBe("https://github.com/btucker/midtown/pull/99");
	});

	it("returns null when no repo info is available", () => {
		const kanban = { review: [], done: [] } as unknown as KanbanData;
		const url = getPrUrl(99, kanban, [], null);
		expect(url).toBeNull();
	});

	it("accepts string PR numbers", () => {
		const kanban = { review: [], done: [] } as unknown as KanbanData;
		const url = getPrUrl("42", kanban, [], primaryRepo);
		expect(url).toBe("https://github.com/btucker/midtown/pull/42");
	});
});

// ── Mobile message tap handler ────────────────────────────────────────────────
// resolveMessageTapAction is the pure decision logic extracted from
// handleMessageTap in Channel.svelte. On mobile, tapping a message row can:
//   - Open a PR on GitHub (data-pr link)
//   - Open a task thread (data-task link)
//   - Open the message thread (default tap)
//   - Do nothing (desktop, thread replies, interactive controls, external links)

describe("resolveMessageTapAction", () => {
	const topLevelMsg = { thread_parent_id: null } as unknown as Message;
	const threadReply = { thread_parent_id: "parent-123" } as unknown as Message;

	// Helper: build a link descriptor for an internal pseudo-link
	function internalLink(dataset: Record<string, string>) {
		return { isExternal: false, dataset };
	}

	// ── Guard conditions (returns null → let event propagate) ──

	it("returns null on wide screen (desktop)", () => {
		const result = resolveMessageTapAction({
			isWideScreen: true,
			msg: topLevelMsg,
			isInteractiveControl: false,
			link: null,
		});
		expect(result).toBeNull();
	});

	it("returns null for thread replies", () => {
		const result = resolveMessageTapAction({
			isWideScreen: false,
			msg: threadReply,
			isInteractiveControl: false,
			link: null,
		});
		expect(result).toBeNull();
	});

	it("returns null when tapping an interactive control", () => {
		const result = resolveMessageTapAction({
			isWideScreen: false,
			msg: topLevelMsg,
			isInteractiveControl: true,
			link: null,
		});
		expect(result).toBeNull();
	});

	it("returns null for external links", () => {
		const result = resolveMessageTapAction({
			isWideScreen: false,
			msg: topLevelMsg,
			isInteractiveControl: false,
			link: { isExternal: true, dataset: {} },
		});
		expect(result).toBeNull();
	});

	// ── PR link handling (key invariant from !2027) ──

	it("returns open_pr action for PR links", () => {
		const result = resolveMessageTapAction({
			isWideScreen: false,
			msg: topLevelMsg,
			isInteractiveControl: false,
			link: internalLink({ pr: "42" }),
		});
		expect(result).toEqual({ type: "open_pr", prNum: "42" });
	});

	it("PR link takes precedence — never opens a task thread", () => {
		// A link could theoretically have both data-pr and data-task.
		// PR behavior must win — PR links always open GitHub (!2027 invariant).
		const result = resolveMessageTapAction({
			isWideScreen: false,
			msg: topLevelMsg,
			isInteractiveControl: false,
			link: internalLink({ pr: "42", task: "7" }),
		});
		expect(result).toEqual({ type: "open_pr", prNum: "42" });
	});

	// ── Task link handling ──

	it("returns open_task action for task links", () => {
		const result = resolveMessageTapAction({
			isWideScreen: false,
			msg: topLevelMsg,
			isInteractiveControl: false,
			link: internalLink({ task: "7" }),
		});
		expect(result).toEqual({ type: "open_task", taskId: "7" });
	});

	// ── Default: open message thread ──

	it("returns open_thread when tapping plain message text", () => {
		const result = resolveMessageTapAction({
			isWideScreen: false,
			msg: topLevelMsg,
			isInteractiveControl: false,
			link: null,
		});
		expect(result).toEqual({ type: "open_thread" });
	});

	it("returns open_thread when tapping an internal link without task/pr", () => {
		// Channel and coworker links are internal pseudo-links that don't
		// have their own mobile handler — they fall through to open_thread.
		const result = resolveMessageTapAction({
			isWideScreen: false,
			msg: topLevelMsg,
			isInteractiveControl: false,
			link: internalLink({ channel: "web" }),
		});
		expect(result).toEqual({ type: "open_thread" });
	});

	it("returns open_thread for coworker links", () => {
		const result = resolveMessageTapAction({
			isWideScreen: false,
			msg: topLevelMsg,
			isInteractiveControl: false,
			link: internalLink({ coworker: "york" }),
		});
		expect(result).toEqual({ type: "open_thread" });
	});
});

// ── Tool block derivation utilities ──────────────────────────────────────────

describe("collectToolBlocks", () => {
	it("returns empty array for messages with no tool_data", () => {
		const msgs = [{ content: "hello" }, { content: "world", tool_data: [] }] as unknown as Message[];
		expect(collectToolBlocks(msgs)).toEqual([]);
	});

	it("collects tool_data blocks from multiple messages", () => {
		const block1 = { call_id: "c1", tool_name: "Read", output: null };
		const block2 = { call_id: "c1", output: "file contents" };
		const block3 = { call_id: "c2", tool_name: "Edit", output: null };
		const msgs = [
			{ content: "msg1", tool_data: [block1, block2] },
			{ content: "msg2" },
			{ content: "msg3", tool_data: [block3] },
		] as unknown as Message[];
		expect(collectToolBlocks(msgs)).toEqual([block1, block2, block3]);
	});
});

describe("hasInProgressToolBlocks", () => {
	it("returns false for empty blocks", () => {
		expect(hasInProgressToolBlocks([])).toBe(false);
	});

	it("returns false when all blocks have output (completed)", () => {
		const blocks = [
			{ call_id: "c1", tool_name: "Read", output: null },
			{ call_id: "c1", output: "result" },
		] as unknown as ToolBlock[];
		expect(hasInProgressToolBlocks(blocks)).toBe(false);
	});

	it("returns true when a block has output === null with no matching completion", () => {
		const blocks = [{ call_id: "c1", tool_name: "Read", output: null }] as ToolBlock[];
		expect(hasInProgressToolBlocks(blocks)).toBe(true);
	});

	it("returns true when one call is completed but another is still in-progress", () => {
		const blocks = [
			{ call_id: "c1", tool_name: "Read", output: null },
			{ call_id: "c1", output: "done" },
			{ call_id: "c2", tool_name: "Edit", output: null },
		] as unknown as ToolBlock[];
		expect(hasInProgressToolBlocks(blocks)).toBe(true);
	});

	it("returns false when blocks lack call_id", () => {
		const blocks = [{ output: null }] as unknown as ToolBlock[];
		expect(hasInProgressToolBlocks(blocks)).toBe(false);
	});
});

describe("getMostRecentToolCall", () => {
	it("returns null for empty blocks", () => {
		expect(getMostRecentToolCall([])).toBeNull();
	});

	it("returns the last tool_name block with InProgress status", () => {
		const blocks = [{ call_id: "c1", tool_name: "Read", output: null }] as ToolBlock[];
		const result = getMostRecentToolCall(blocks);
		expect(result).toEqual({ toolName: "Read", callId: "c1", status: "InProgress" });
	});

	it("returns ok status when a matching result exists", () => {
		const blocks = [
			{ call_id: "c1", tool_name: "Read", output: null },
			{ call_id: "c1", output: "result" },
		] as unknown as ToolBlock[];
		const result = getMostRecentToolCall(blocks);
		expect(result).toEqual({ toolName: "Read", callId: "c1", status: "ok" });
	});

	it("returns error status when result has error flag", () => {
		const blocks = [
			{ call_id: "c1", tool_name: "Read", output: null },
			{ call_id: "c1", output: "failed", error: true },
		] as unknown as ToolBlock[];
		const result = getMostRecentToolCall(blocks);
		expect(result).toEqual({ toolName: "Read", callId: "c1", status: "error" });
	});

	it("returns the most recent tool call when multiple exist", () => {
		const blocks = [
			{ call_id: "c1", tool_name: "Read", output: null },
			{ call_id: "c1", output: "done" },
			{ call_id: "c2", tool_name: "Edit", output: null },
		] as unknown as ToolBlock[];
		const result = getMostRecentToolCall(blocks);
		expect(result).toEqual({ toolName: "Edit", callId: "c2", status: "InProgress" });
	});
});

describe("getChannelThreads excludes completed threads", () => {
	const tracked: Record<string, TrackedThread> = {
		"thread-1": { channelName: "web", subject: "Active thread", lastActivity: "2026-03-04T10:00:00Z", replyCount: 0 },
		"completed-thread": {
			channelName: "web",
			subject: "Completed thread",
			lastActivity: "2026-03-04T11:00:00Z",
			replyCount: 0,
		},
	};
	const unreadCounts: Record<string, number> = {};
	const taskThreadIds = new Set<string>();
	const completedTaskThreadIds = new Set(["completed-thread"]);

	it("filters out completed threads from per-channel list", () => {
		const result = getChannelThreads("web", tracked, unreadCounts, taskThreadIds, completedTaskThreadIds);
		expect(result.threads).toHaveLength(1);
		expect(result.threads[0].id).toBe("thread-1");
	});

	it("includes completed threads when completedTaskThreadIds is empty", () => {
		const result = getChannelThreads("web", tracked, unreadCounts, taskThreadIds, new Set());
		expect(result.threads).toHaveLength(2);
	});
});

describe("getAllCompletedThreads", () => {
	const tracked: Record<string, TrackedThread> = {
		"thread-1": { channelName: "web", subject: "Active thread", lastActivity: "2026-03-04T10:00:00Z", replyCount: 0 },
		"completed-1": {
			channelName: "web",
			subject: "Done task A",
			fullText: "Done task A full",
			lastActivity: "2026-03-04T11:00:00Z",
			replyCount: 5,
		},
		"completed-2": {
			channelName: "auth",
			subject: "Done task B",
			lastActivity: "2026-03-04T12:00:00Z",
			replyCount: 0,
		},
		"task-thread": {
			channelName: "web",
			subject: "Active task thread",
			lastActivity: "2026-03-04T13:00:00Z",
			replyCount: 0,
		},
	};
	const unreadCounts = { "completed-1": 3 };
	const taskThreadIds = new Set(["task-thread"]);
	const completedTaskThreadIds = new Set(["completed-1", "completed-2"]);

	it("returns only completed threads across all channels", () => {
		const result = getAllCompletedThreads(tracked, unreadCounts, taskThreadIds, completedTaskThreadIds);
		expect(result).toHaveLength(2);
		const ids = result.map((t) => t.id);
		expect(ids).toContain("completed-1");
		expect(ids).toContain("completed-2");
	});

	it("sorts by lastActivity newest first", () => {
		const result = getAllCompletedThreads(tracked, unreadCounts, taskThreadIds, completedTaskThreadIds);
		expect(result[0].id).toBe("completed-2"); // 12:00
		expect(result[1].id).toBe("completed-1"); // 11:00
	});

	it("includes channelName on each thread", () => {
		const result = getAllCompletedThreads(tracked, unreadCounts, taskThreadIds, completedTaskThreadIds);
		const c1 = result.find((t) => t.id === "completed-1");
		const c2 = result.find((t) => t.id === "completed-2");
		expect(c1?.channelName).toBe("web");
		expect(c2?.channelName).toBe("auth");
	});

	it("includes unread counts", () => {
		const result = getAllCompletedThreads(tracked, unreadCounts, taskThreadIds, completedTaskThreadIds);
		const c1 = result.find((t) => t.id === "completed-1");
		expect(c1?.unread).toBe(3);
	});

	it("excludes active task-backed threads even if also in completedTaskThreadIds", () => {
		// If a thread is in both sets, taskThreadIds takes priority (active task)
		const bothSets = new Set(["task-thread", "completed-1", "completed-2"]);
		const result = getAllCompletedThreads(tracked, unreadCounts, taskThreadIds, bothSets);
		const ids = result.map((t) => t.id);
		expect(ids).not.toContain("task-thread");
		expect(result).toHaveLength(2);
	});

	it("returns empty array when no completed threads exist", () => {
		const result = getAllCompletedThreads(tracked, unreadCounts, taskThreadIds, new Set());
		expect(result).toEqual([]);
	});
});

describe("getChannelHasTrackedThreads with completedTaskThreadIds", () => {
	const tracked: Record<string, TrackedThread> = {
		"thread-1": { channelName: "web", subject: "Active thread", lastActivity: "", replyCount: 0 },
		"completed-thread": { channelName: "web", subject: "Completed thread", lastActivity: "", replyCount: 0 },
		"other-thread": { channelName: "auth", subject: "Other thread", lastActivity: "", replyCount: 0 },
	};
	const taskThreadIds = new Set<string>();
	const completedTaskThreadIds = new Set(["completed-thread"]);

	it("returns true when channel has non-completed tracked threads", () => {
		expect(getChannelHasTrackedThreads("web", tracked, taskThreadIds, completedTaskThreadIds)).toBe(true);
	});

	it("returns false when all channel threads are completed", () => {
		const onlyCompleted: Record<string, TrackedThread> = {
			"completed-thread": { channelName: "web", subject: "Completed thread", lastActivity: "", replyCount: 0 },
		};
		expect(getChannelHasTrackedThreads("web", onlyCompleted, taskThreadIds, completedTaskThreadIds)).toBe(false);
	});

	it("returns true without completedTaskThreadIds (backward compatible)", () => {
		expect(getChannelHasTrackedThreads("web", tracked, taskThreadIds)).toBe(true);
	});
});
