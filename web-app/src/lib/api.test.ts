import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
	clearErrorCallback,
	closeThread,
	fetchChannelAgentsMd,
	fetchChannels,
	fetchHistory,
	forkThread,
	handleUpdate,
	onNextError,
	pushNavState,
	selectDm,
	switchProject,
	unforkThread,
} from "./api.ts";
import {
	activeChannel,
	activeProject,
	channels,
	dismissedThreads,
	messagesByChannel,
	threadData,
	threadUnreadCounts,
	trackedThreads,
	userSenderName,
} from "./store.ts";
import type { Message } from "./types.ts";

describe("fetchHistory", () => {
	let originalFetch: typeof globalThis.fetch;

	beforeEach(() => {
		originalFetch = globalThis.fetch;
		// Reset store to known state: channel-a has existing messages
		messagesByChannel.set({
			midtown: [],
			"channel-a": [{ id: "1", content: "existing message", channel: "channel-a" } as Message],
		});
	});

	afterEach(() => {
		globalThis.fetch = originalFetch;
	});

	it("preserves existing channel messages when bulk fetch only returns other channels", async () => {
		// Regression: fetchHistory() (no param) called on WS reconnect was doing
		// messagesByChannel.set(byChannel) which wiped channels not in the response.
		// If the server only returns messages for channel-b, channel-a should NOT be cleared.
		globalThis.fetch = vi.fn().mockResolvedValue({
			ok: true,
			json: async () => [{ id: 2, content: "new message", channel: "channel-b", timestamp: "2026-01-01T00:00:00Z" }],
		});

		await fetchHistory();

		const store = get(messagesByChannel);
		// channel-b should have the new message
		expect(store["channel-b"]).toHaveLength(1);
		// channel-a must NOT have been wiped
		expect(store["channel-a"]).toHaveLength(1);
		expect(store["channel-a"][0].content).toBe("existing message");
	});

	it("updates existing channel data when bulk fetch returns fresh messages for that channel", async () => {
		globalThis.fetch = vi.fn().mockResolvedValue({
			ok: true,
			json: async () => [{ id: 10, content: "fresh message", channel: "channel-a", timestamp: "2026-01-01T00:00:00Z" }],
		});

		await fetchHistory();

		const store = get(messagesByChannel);
		// channel-a should have the fresh data (overriding the old single message)
		expect(store["channel-a"]).toHaveLength(1);
		expect(store["channel-a"][0].content).toBe("fresh message");
	});

	it("clears ghost pending messages from channels absent in the history response", async () => {
		// Regression: if the WS echo was lost during a disconnect, a pending optimistic
		// message can survive in a low-traffic channel that doesn't appear in the bulk
		// history response. The merge-not-replace strategy preserves the channel, so the
		// pending message lingers forever as a "ghost".
		//
		// Fix: strip pending messages from all existing channels before merging, so
		// only confirmed (non-pending) messages remain for channels not in the response.
		messagesByChannel.set({
			midtown: [],
			web: [
				{ id: "real-1", content: "existing confirmed", channel: "web", from: "coworker" } as Message,
				{ id: "pending-ghost", content: "my unsent msg", channel: "web", from: "user", pending: true } as Message,
			],
		});

		// Bulk fetch only returns midtown — 'web' is low-traffic, not in response
		globalThis.fetch = vi.fn().mockResolvedValue({
			ok: true,
			json: async () => [{ id: 99, content: "midtown msg", channel: "midtown", timestamp: "2026-01-01T00:00:00Z" }],
		});

		await fetchHistory();

		const store = get(messagesByChannel);
		expect(store.midtown).toHaveLength(1);
		// The confirmed message should still be there
		expect(store.web.find((m) => m.id === "real-1")).toBeTruthy();
		// The pending ghost must be gone
		expect(store.web.some((m) => m.pending)).toBe(false);
		expect(store.web.find((m) => m.id === "pending-ghost")).toBeUndefined();
	});

	it("retains existing channel messages when single-channel fetch returns empty", async () => {
		// Regression (!1968): fetchHistory('channel-a') called on channel switch would
		// replace channel-a's messages with an empty array if the backend returned [].
		// This wipes the channel display — only new WS messages appear until the next
		// successful fetch. Fix: skip the store update when the response is empty but
		// the store already has messages (same "retain last-known-good" pattern as the
		// catch block).
		globalThis.fetch = vi.fn().mockResolvedValue({
			ok: true,
			json: async () => [],
		});

		await fetchHistory("channel-a");

		const store = get(messagesByChannel);
		// channel-a must NOT have been wiped
		expect(store["channel-a"]).toHaveLength(1);
		expect(store["channel-a"][0].content).toBe("existing message");
	});

	it("populates a new channel with messages from single-channel fetch", async () => {
		// Normal population: a channel with no existing messages gets populated
		// by the fetch response. The empty-response guard does not interfere
		// because there is no existing data to retain.
		globalThis.fetch = vi.fn().mockResolvedValue({
			ok: true,
			json: async () => [{ id: 5, content: "new msg", channel: "new-channel", timestamp: "2026-01-01T00:00:00Z" }],
		});

		await fetchHistory("new-channel");

		const store = get(messagesByChannel);
		expect(store["new-channel"]).toHaveLength(1);
		expect(store["new-channel"][0].content).toBe("new msg");
	});

	it("allows empty response to pass through when channel has no existing messages", async () => {
		// When a channel has no cached messages, an empty response should set the
		// channel to [] without being blocked by the guard. The guard only retains
		// data when there IS existing data to protect.
		globalThis.fetch = vi.fn().mockResolvedValue({
			ok: true,
			json: async () => [],
		});

		await fetchHistory("brand-new-channel");

		const store = get(messagesByChannel);
		expect(store["brand-new-channel"]).toEqual([]);
	});

	it("updates store when response contains only thread replies (post-filter empty)", async () => {
		// If the backend returns messages that are all thread replies,
		// annotateThreadReplyCounts filters them out, producing channelMsgs=[].
		// This is real data (data.length > 0), not a transient empty response,
		// so the guard must NOT retain stale cached data.
		globalThis.fetch = vi.fn().mockResolvedValue({
			ok: true,
			json: async () => [
				{
					id: 20,
					content: "thread reply",
					channel: "channel-a",
					thread_parent_id: "parent-1",
					timestamp: "2026-01-01T00:00:00Z",
				},
			],
		});

		await fetchHistory("channel-a");

		const store = get(messagesByChannel);
		// channel-a should be updated (not retained) — the response was non-empty
		// even though all messages were thread replies filtered by annotateThreadReplyCounts
		expect(store["channel-a"]).toEqual([]);
	});

	it("strips pending messages from retained data on empty response", async () => {
		// When the guard retains existing data, pending (optimistic) messages must
		// be stripped to avoid ghost messages lingering indefinitely (matching the
		// bulk-fetch path's cleanup behavior).
		messagesByChannel.set({
			midtown: [],
			"channel-a": [
				{ id: "1", content: "confirmed msg", channel: "channel-a", from: "coworker" } as Message,
				{ id: "pending-xyz", content: "my unsent msg", channel: "channel-a", from: "user", pending: true } as Message,
			],
		});

		globalThis.fetch = vi.fn().mockResolvedValue({
			ok: true,
			json: async () => [],
		});

		await fetchHistory("channel-a");

		const store = get(messagesByChannel);
		// Confirmed message retained, pending message stripped
		expect(store["channel-a"]).toHaveLength(1);
		expect(store["channel-a"][0].id).toBe("1");
		expect(store["channel-a"].some((m) => m.pending)).toBe(false);
	});

	it("does not retain when all existing messages are pending and response is empty", async () => {
		// If the channel only has pending messages and the backend returns empty,
		// there's no confirmed data to retain — allow the empty response through.
		messagesByChannel.set({
			midtown: [],
			"channel-a": [
				{ id: "pending-1", content: "unsent", channel: "channel-a", from: "user", pending: true } as Message,
			],
		});

		globalThis.fetch = vi.fn().mockResolvedValue({
			ok: true,
			json: async () => [],
		});

		await fetchHistory("channel-a");

		const store = get(messagesByChannel);
		expect(store["channel-a"]).toEqual([]);
	});

	it("replaces existing channel messages when single-channel fetch returns non-empty data", async () => {
		// Normal case: when the backend returns fresh (non-empty) data, it should
		// replace the existing messages — this is the expected "refresh" behavior.
		globalThis.fetch = vi.fn().mockResolvedValue({
			ok: true,
			json: async () => [
				{ id: 10, content: "refreshed message", channel: "channel-a", timestamp: "2026-01-01T00:00:00Z" },
			],
		});

		await fetchHistory("channel-a");

		const store = get(messagesByChannel);
		expect(store["channel-a"]).toHaveLength(1);
		expect(store["channel-a"][0].content).toBe("refreshed message");
	});

	it("does not leave pending messages when the channel is included in the history response", async () => {
		// When a channel IS in the bulk response, its data replaces existing entirely.
		// Pending messages in existing are discarded because the whole channel array
		// is overwritten — this is the already-correct baseline behavior.
		messagesByChannel.set({
			midtown: [{ id: "pending-mt", content: "hello", channel: "midtown", from: "user", pending: true } as Message],
		});

		globalThis.fetch = vi.fn().mockResolvedValue({
			ok: true,
			json: async () => [
				{ id: 50, content: "confirmed midtown", channel: "midtown", timestamp: "2026-01-01T00:00:00Z" },
			],
		});

		await fetchHistory();

		const store = get(messagesByChannel);
		expect(store.midtown).toHaveLength(1);
		expect(store.midtown[0].id).toBe(50);
		expect(store.midtown.some((m) => m.pending)).toBe(false);
	});
});

describe("fetchHistory — AbortController cancellation", () => {
	let originalFetch: typeof globalThis.fetch;

	beforeEach(() => {
		originalFetch = globalThis.fetch;
		messagesByChannel.set({
			midtown: [],
			"channel-a": [{ id: "1", content: "existing", channel: "channel-a" } as Message],
		});
	});

	afterEach(() => {
		globalThis.fetch = originalFetch;
	});

	it("aborts a previous in-flight request when a new one starts for the same channel", async () => {
		const abortSignals: AbortSignal[] = [];
		globalThis.fetch = vi.fn().mockImplementation((_url: string, opts?: RequestInit) => {
			if (opts?.signal) abortSignals.push(opts.signal);
			return new Promise((resolve) => setTimeout(() => resolve({ ok: true, json: async () => [] }), 100));
		});

		// Start two concurrent fetches for the same channel
		const first = fetchHistory("channel-a");
		const second = fetchHistory("channel-a");

		await Promise.allSettled([first, second]);

		// The first request's signal should have been aborted
		expect(abortSignals).toHaveLength(2);
		expect(abortSignals[0].aborted).toBe(true);
		expect(abortSignals[1].aborted).toBe(false);
	});

	it("does not abort requests for different channels", async () => {
		const abortSignals: Record<string, AbortSignal> = {};
		globalThis.fetch = vi.fn().mockImplementation((url: string, opts?: RequestInit) => {
			const ch = url.includes("channel=") ? new URL(url, "http://localhost").searchParams.get("channel") : "__all__";
			if (ch && opts?.signal) abortSignals[ch] = opts.signal;
			return Promise.resolve({ ok: true, json: async () => [] });
		});

		await Promise.all([fetchHistory("channel-a"), fetchHistory("channel-b")]);

		expect(abortSignals["channel-a"].aborted).toBe(false);
		expect(abortSignals["channel-b"].aborted).toBe(false);
	});
});

describe("handleUpdate — optimistic message deduplication", () => {
	beforeEach(() => {
		messagesByChannel.set({ midtown: [] });
		threadData.set(null);
	});

	it("replaces a pending optimistic message with the real server message", () => {
		// Simulate user hitting Send: a pending placeholder is in the store
		messagesByChannel.set({
			midtown: [{ id: "pending-abc", from: "user", content: "hello", channel: "midtown", pending: true } as Message],
		});

		// Server echoes back the real message
		handleUpdate({
			type: "channel_message",
			data: { id: "real-1", from: "user", content: "hello", channel: "midtown", timestamp: "2026-01-01T00:00:00Z" },
		});

		const store = get(messagesByChannel);
		// The pending placeholder should be gone; only the real message remains
		expect(store.midtown).toHaveLength(1);
		expect(store.midtown[0].id).toBe("real-1");
		expect(store.midtown[0].pending).toBeUndefined();
	});

	it("does not remove a pending message if content does not match", () => {
		messagesByChannel.set({
			midtown: [
				{
					id: "pending-abc",
					from: "user",
					content: "different text",
					channel: "midtown",
					pending: true,
				} as Message,
			],
		});

		// Real message arrives for a different content
		handleUpdate({
			type: "channel_message",
			data: { id: "real-2", from: "user", content: "hello", channel: "midtown", timestamp: "2026-01-01T00:00:00Z" },
		});

		const store = get(messagesByChannel);
		// Both messages should be present: the unmatched pending + the real one
		expect(store.midtown).toHaveLength(2);
		expect(store.midtown.some((m) => m.pending)).toBe(true);
		expect(store.midtown.some((m) => m.id === "real-2")).toBe(true);
	});

	it("only removes the first matching pending message when duplicates exist", () => {
		messagesByChannel.set({
			midtown: [
				{ id: "pending-1", from: "user", content: "hello", channel: "midtown", pending: true } as Message,
				{ id: "pending-2", from: "user", content: "hello", channel: "midtown", pending: true } as Message,
			],
		});

		handleUpdate({
			type: "channel_message",
			data: { id: "real-3", from: "user", content: "hello", channel: "midtown", timestamp: "2026-01-01T00:00:00Z" },
		});

		const store = get(messagesByChannel);
		// First pending removed, second pending preserved, real message appended
		expect(store.midtown).toHaveLength(2);
		expect(store.midtown[0].id).toBe("pending-2");
		expect(store.midtown[1].id).toBe("real-3");
	});

	it("replaces a pending thread reply with the real server reply", () => {
		const parentId = "parent-msg-1";
		threadData.set({
			parentMessage: { id: parentId, from: "lead", content: "original" } as Message,
			channelName: "midtown",
			messages: [{ id: "pending-reply-1", from: "user", content: "my reply", pending: true } as Message],
			tasks: [],
		});

		// Server echoes the real thread reply
		handleUpdate({
			type: "channel_message",
			data: {
				id: "real-reply-1",
				from: "user",
				content: "my reply",
				channel: "midtown",
				thread_parent_id: parentId,
				timestamp: "2026-01-01T00:00:00Z",
			},
		});

		const td = get(threadData)!;
		expect(td.messages).toHaveLength(1);
		expect(td.messages[0].id).toBe("real-reply-1");
		expect(td.messages[0].pending).toBeUndefined();
	});

	it("only removes the first matching pending thread reply when duplicates exist", () => {
		const parentId = "parent-msg-1";
		threadData.set({
			parentMessage: { id: parentId, from: "lead", content: "original" } as Message,
			channelName: "midtown",
			messages: [
				{ id: "pending-t1", from: "user", content: "same text", pending: true } as Message,
				{ id: "pending-t2", from: "user", content: "same text", pending: true } as Message,
			],
			tasks: [],
		});

		handleUpdate({
			type: "channel_message",
			data: {
				id: "real-thread-reply",
				from: "user",
				content: "same text",
				channel: "midtown",
				thread_parent_id: parentId,
				timestamp: "2026-01-01T00:00:00Z",
			},
		});

		const td = get(threadData)!;
		// First pending removed, second preserved, real appended
		expect(td.messages).toHaveLength(2);
		expect(td.messages[0].id).toBe("pending-t2");
		expect(td.messages[1].id).toBe("real-thread-reply");
	});

	it("does not remove a pending message when a different user posts the same content", () => {
		messagesByChannel.set({
			midtown: [{ id: "pending-mine", from: "user", content: "hello", channel: "midtown", pending: true } as Message],
		});

		// Another participant sends identical content before the user's echo arrives
		handleUpdate({
			type: "channel_message",
			data: {
				id: "other-user-msg",
				from: "alice",
				content: "hello",
				channel: "midtown",
				timestamp: "2026-01-01T00:00:00Z",
			},
		});

		const store = get(messagesByChannel);
		// Pending placeholder must NOT be consumed by another user's message
		expect(store.midtown).toHaveLength(2);
		expect(store.midtown.some((m) => m.pending)).toBe(true);
		expect(store.midtown.some((m) => m.id === "other-user-msg")).toBe(true);
	});

	it("does not modify threadData when the panel is for a different parent", () => {
		const parentId = "parent-msg-1";
		const otherParentId = "parent-msg-2";
		threadData.set({
			parentMessage: { id: otherParentId, from: "lead", content: "other thread" } as Message,
			channelName: "midtown",
			messages: [{ id: "pending-reply-2", from: "user", content: "my reply", pending: true } as Message],
			tasks: [],
		});

		handleUpdate({
			type: "channel_message",
			data: {
				id: "real-reply-2",
				from: "user",
				content: "my reply",
				channel: "midtown",
				thread_parent_id: parentId, // different parent
				timestamp: "2026-01-01T00:00:00Z",
			},
		});

		const td = get(threadData)!;
		// Panel is for otherParentId — should be untouched
		expect(td.messages).toHaveLength(1);
		expect(td.messages[0].id).toBe("pending-reply-2");
	});
});

describe("fetchChannels — is_dm field", () => {
	let originalFetch: typeof globalThis.fetch;

	beforeEach(() => {
		originalFetch = globalThis.fetch;
		channels.set([{ name: "midtown", unread: 0, has_pr: false, ci_status: null }]);
	});

	afterEach(() => {
		globalThis.fetch = originalFetch;
	});

	it("propagates is_dm=true from the API response", async () => {
		globalThis.fetch = vi.fn().mockResolvedValue({
			ok: true,
			json: async () => ({
				channels: [
					{ name: "midtown", is_archived: false, is_dm: false },
					{ name: "dm-alice", is_archived: false, is_dm: true },
				],
			}),
		});

		await fetchChannels();

		const ch = get(channels);
		const dmChannel = ch.find((c) => c.name === "dm-alice");
		expect(dmChannel).toBeTruthy();
		expect(dmChannel?.is_dm).toBe(true);
		expect(ch.find((c) => c.name === "midtown")?.is_dm).toBe(false);
	});

	it("defaults is_dm to false for string-format channels (legacy API)", async () => {
		globalThis.fetch = vi.fn().mockResolvedValue({
			ok: true,
			json: async () => ({ channels: ["midtown", "other"] }),
		});

		await fetchChannels();

		const ch = get(channels);
		expect(ch.every((c) => c.is_dm === false)).toBe(true);
	});
});

describe("selectDm", () => {
	let originalFetch: typeof globalThis.fetch;
	let originalHistory: typeof globalThis.history;

	beforeEach(() => {
		originalFetch = globalThis.fetch;
		// Mock browser history API (not available in Node test environment)
		originalHistory = globalThis.history;
		globalThis.history = { pushState: vi.fn(), replaceState: vi.fn() } as unknown as History;
		channels.set([
			{ name: "midtown", unread: 0, has_pr: false, ci_status: null, is_dm: false },
			{ name: "dm-alice", unread: 3, has_pr: false, ci_status: null, is_dm: true },
		]);
		activeChannel.set("midtown");
		messagesByChannel.set({
			midtown: [],
			"dm-alice": [{ id: "1", content: "hey", channel: "dm-alice" } as Message],
		});
	});

	afterEach(() => {
		globalThis.fetch = originalFetch;
		globalThis.history = originalHistory;
	});

	it("switches to existing DM channel without creating it", async () => {
		const fetchMock = vi.fn().mockResolvedValue({
			ok: true,
			json: async () => [{ id: 1, content: "hey", channel: "dm-alice" }],
		});
		globalThis.fetch = fetchMock;

		await selectDm("alice");

		// No create call should have been made — only a fetchHistory call
		expect(fetchMock).toHaveBeenCalledTimes(1);
		expect(fetchMock.mock.calls[0][0]).toContain("/channels/history");
		expect(get(activeChannel)).toBe("dm-alice");
	});

	it("clears the unread count when switching to a DM channel", async () => {
		globalThis.fetch = vi.fn();

		await selectDm("alice");

		const ch = get(channels).find((c) => c.name === "dm-alice");
		expect(ch?.unread).toBe(0);
	});

	it("creates the DM channel if it does not exist, then switches to it", async () => {
		channels.set([{ name: "midtown", unread: 0, has_pr: false, ci_status: null, is_dm: false }]);

		globalThis.fetch = vi
			.fn()
			// First call: POST create
			.mockResolvedValueOnce({ ok: true, json: async () => ({}) })
			// Second call: GET fetchChannels
			.mockResolvedValueOnce({
				ok: true,
				json: async () => ({
					channels: [
						{ name: "midtown", is_archived: false, is_dm: false },
						{ name: "dm-bob", is_archived: false, is_dm: true },
					],
				}),
			})
			// Third call: GET fetchHistory for dm-bob
			.mockResolvedValueOnce({ ok: true, json: async () => [] });

		await selectDm("bob");

		expect(get(activeChannel)).toBe("dm-bob");
		// Create endpoint should have been called with the right name
		const createCall = (globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
		expect(createCall[0]).toContain("/channels/create");
		expect(JSON.parse(createCall[1].body).name).toBe("dm-bob");
	});

	it("still navigates to DM channel when backend creation returns an error", async () => {
		// Regression: selectDm used to call `return` after a non-ok response, leaving
		// activeChannel unchanged and giving the user no visible feedback.
		channels.set([{ name: "midtown", unread: 0, has_pr: false, ci_status: null, is_dm: false }]);

		globalThis.fetch = vi.fn().mockResolvedValueOnce({
			ok: false,
			json: async () => ({ error: "internal server error" }),
		});

		await selectDm("carol");

		// Despite the backend failure, the user should land on the DM channel
		expect(get(activeChannel)).toBe("dm-carol");
		// The channel should be in the sidebar as a DM
		const ch = get(channels).find((c) => c.name === "dm-carol");
		expect(ch).toBeTruthy();
		expect(ch?.is_dm).toBe(true);
	});

	it("still navigates to DM channel when fetchChannels fails after creation", async () => {
		// Regression: selectDm used to call `return` inside the catch block when
		// fetchChannels threw, leaving activeChannel unchanged.
		channels.set([{ name: "midtown", unread: 0, has_pr: false, ci_status: null, is_dm: false }]);

		globalThis.fetch = vi
			.fn()
			.mockResolvedValueOnce({ ok: true, json: async () => ({}) }) // create succeeds
			.mockRejectedValueOnce(new Error("network error")); // fetchChannels fails

		await selectDm("dave");

		expect(get(activeChannel)).toBe("dm-dave");
		const ch = get(channels).find((c) => c.name === "dm-dave");
		expect(ch).toBeTruthy();
		expect(ch?.is_dm).toBe(true);
	});
});

describe("fetchChannels — is_dm name-prefix fallback", () => {
	let originalFetch: typeof globalThis.fetch;

	beforeEach(() => {
		originalFetch = globalThis.fetch;
		channels.set([{ name: "midtown", unread: 0, has_pr: false, ci_status: null }]);
	});

	afterEach(() => {
		globalThis.fetch = originalFetch;
	});

	it("marks dm- prefixed channels as DMs when is_dm field is absent from API response", async () => {
		// Regression: if the backend omits is_dm (or sends undefined) for a dm-* channel,
		// fetchChannels stored is_dm=undefined (falsy), and ChannelList filtered it out
		// so the DM section never appeared.
		globalThis.fetch = vi.fn().mockResolvedValue({
			ok: true,
			json: async () => ({
				channels: [
					{ name: "midtown", is_archived: false, is_dm: false },
					{ name: "dm-eve", is_archived: false }, // no is_dm field
				],
			}),
		});

		await fetchChannels();

		const ch = get(channels).find((c) => c.name === "dm-eve");
		expect(ch).toBeTruthy();
		expect(ch?.is_dm).toBe(true);
	});
});

describe("switchProject — initial channel state uses project name, not hardcoded midtown", () => {
	beforeEach(() => {
		channels.set([]);
		activeChannel.set(null);
		messagesByChannel.set({});
	});

	it("sets activeChannel to the project name", () => {
		switchProject("my-project", null);
		expect(get(activeChannel)).toBe("my-project");
	});

	it("initializes messagesByChannel keyed by project name", () => {
		switchProject("my-project", null);
		const store = get(messagesByChannel);
		expect(Object.keys(store)).toContain("my-project");
		expect(Object.keys(store)).not.toContain("midtown");
	});

	it("initializes channels list with the project name, not midtown", () => {
		switchProject("my-project", null);
		const ch = get(channels);
		expect(ch.some((c) => c.name === "my-project")).toBe(true);
		expect(ch.some((c) => c.name === "midtown")).toBe(false);
	});
});

describe("fetchHistory — channelless messages use activeProject, not hardcoded midtown", () => {
	let originalFetch: typeof globalThis.fetch;

	beforeEach(() => {
		originalFetch = globalThis.fetch;
		activeProject.set("my-project");
		messagesByChannel.set({ "my-project": [] });
	});

	afterEach(() => {
		globalThis.fetch = originalFetch;
		activeProject.set(null);
	});

	it("buckets a message with no channel field under activeProject, not midtown", async () => {
		globalThis.fetch = vi.fn().mockResolvedValue({
			ok: true,
			json: async () => [{ id: 1, content: "hello", from: "lead", timestamp: "2026-01-01T00:00:00Z" }],
		});

		await fetchHistory();

		const store = get(messagesByChannel);
		expect(store["my-project"]).toHaveLength(1);
		expect(store.midtown).toBeUndefined();
	});
});

describe("handleUpdate channel_message — channelless messages route to activeProject", () => {
	beforeEach(() => {
		activeProject.set("my-project");
		messagesByChannel.set({ "my-project": [] });
		threadData.set(null);
	});

	afterEach(() => {
		activeProject.set(null);
	});

	it("routes a message with no channel field to activeProject, not midtown", () => {
		handleUpdate({
			type: "channel_message",
			data: { id: "msg-1", from: "lead", content: "hello", timestamp: "2026-01-01T00:00:00Z" },
		});

		const store = get(messagesByChannel);
		expect(store["my-project"]).toHaveLength(1);
		expect(store.midtown).toBeUndefined();
	});
});

describe("pushNavState — URL construction", () => {
	let originalHistory: typeof globalThis.history;

	beforeEach(() => {
		originalHistory = globalThis.history;
		globalThis.history = { pushState: vi.fn(), replaceState: vi.fn() } as unknown as History;
		activeProject.set("myproject");
	});

	afterEach(() => {
		globalThis.history = originalHistory;
		activeProject.set(null);
	});

	it("includes channel param in URL when thread is on the default channel", () => {
		pushNavState({ channel: "myproject", thread: "msg-123" });
		const [, , url] = (globalThis.history.pushState as ReturnType<typeof vi.fn>).mock.calls[0];
		const parsed = new URL(url, "http://localhost");
		expect(parsed.searchParams.get("channel")).toBe("myproject");
		expect(parsed.searchParams.get("thread")).toBe("msg-123");
	});

	it("omits channel param when no thread and channel matches project", () => {
		pushNavState({ channel: "myproject" });
		const [, , url] = (globalThis.history.pushState as ReturnType<typeof vi.fn>).mock.calls[0];
		const parsed = new URL(url, "http://localhost");
		expect(parsed.searchParams.get("channel")).toBeNull();
		expect(parsed.pathname).toBe("/myproject");
	});

	it("includes channel param when channel differs from project", () => {
		pushNavState({ channel: "other-channel" });
		const [, , url] = (globalThis.history.pushState as ReturnType<typeof vi.fn>).mock.calls[0];
		const parsed = new URL(url, "http://localhost");
		expect(parsed.searchParams.get("channel")).toBe("other-channel");
	});
});

describe("closeThread — history push behavior", () => {
	let originalHistory: typeof globalThis.history;

	beforeEach(() => {
		originalHistory = globalThis.history;
		globalThis.history = { pushState: vi.fn(), replaceState: vi.fn() } as unknown as History;
		activeProject.set("myproject");
		activeChannel.set("myproject");
		threadData.set({
			parentMessage: { id: "msg-1" } as Message,
			channelName: "myproject",
			messages: [],
			tasks: [],
		});
	});

	afterEach(() => {
		globalThis.history = originalHistory;
		activeProject.set(null);
		threadData.set(null);
	});

	it("pushes history entry by default", () => {
		closeThread();
		expect(get(threadData)).toBeNull();
		expect(globalThis.history.pushState).toHaveBeenCalled();
	});

	it("does not push history when pushState is false", () => {
		closeThread({ pushState: false });
		expect(get(threadData)).toBeNull();
		expect(globalThis.history.pushState).not.toHaveBeenCalled();
	});
});

describe("handleUpdate — auto-track threads when someone replies to user message", () => {
	const parentId = "user-msg-1";

	beforeEach(() => {
		trackedThreads.set({});
		dismissedThreads.set(new Set());
		threadUnreadCounts.set({});
		threadData.set(null);
		userSenderName.set("user");
		messagesByChannel.set({
			web: [
				{ id: parentId, from: "user", content: "my question about auth", channel: "web" } as Message,
				{ id: "other-msg", from: "lead", content: "some announcement", channel: "web" } as Message,
			],
		});
	});

	it("auto-tracks a thread when someone replies to a user message", () => {
		handleUpdate({
			type: "channel_message",
			data: {
				id: "reply-1",
				from: "coworker",
				content: "here is the answer",
				channel: "web",
				thread_parent_id: parentId,
				timestamp: "2026-01-01T00:00:00Z",
			},
		});

		const tracked = get(trackedThreads);
		expect(tracked[parentId]).toBeTruthy();
		expect(tracked[parentId].channelName).toBe("web");
		expect(tracked[parentId].subject).toContain("my question about auth");
		// replyCount should match the parent's reply_count (1 after the WS handler
		// incremented it), not double-count from the subsequent update block.
		expect(tracked[parentId].replyCount).toBe(1);
	});

	it("does not double-count replyCount on first auto-tracked reply", () => {
		// Send two replies — replyCount should be exactly 2, not 3 or 4
		handleUpdate({
			type: "channel_message",
			data: {
				id: "reply-a",
				from: "coworker",
				content: "first reply",
				channel: "web",
				thread_parent_id: parentId,
				timestamp: "2026-01-01T00:00:00Z",
			},
		});
		handleUpdate({
			type: "channel_message",
			data: {
				id: "reply-b",
				from: "coworker",
				content: "second reply",
				channel: "web",
				thread_parent_id: parentId,
				timestamp: "2026-01-01T00:00:01Z",
			},
		});

		const tracked = get(trackedThreads);
		expect(tracked[parentId].replyCount).toBe(2);
	});

	it("increments unread count on the first auto-tracked reply", () => {
		handleUpdate({
			type: "channel_message",
			data: {
				id: "reply-1",
				from: "coworker",
				content: "answer",
				channel: "web",
				thread_parent_id: parentId,
				timestamp: "2026-01-01T00:00:00Z",
			},
		});

		const unreads = get(threadUnreadCounts);
		expect(unreads[parentId]).toBe(1);
	});

	it("does not auto-track when the parent message is not from the user", () => {
		handleUpdate({
			type: "channel_message",
			data: {
				id: "reply-2",
				from: "coworker",
				content: "reply to lead",
				channel: "web",
				thread_parent_id: "other-msg",
				timestamp: "2026-01-01T00:00:00Z",
			},
		});

		const tracked = get(trackedThreads);
		expect(tracked["other-msg"]).toBeUndefined();
	});

	it("does not auto-track when the user replies to their own message", () => {
		handleUpdate({
			type: "channel_message",
			data: {
				id: "reply-3",
				from: "user",
				content: "my own follow up",
				channel: "web",
				thread_parent_id: parentId,
				timestamp: "2026-01-01T00:00:00Z",
			},
		});

		const tracked = get(trackedThreads);
		// The user's own reply does not trigger auto-tracking (the outer guard
		// requires msg.from !== 'user')
		expect(tracked[parentId]).toBeUndefined();
	});

	it("respects dismissedThreads — does not re-track a dismissed thread", () => {
		dismissedThreads.set(new Set([parentId]));

		handleUpdate({
			type: "channel_message",
			data: {
				id: "reply-4",
				from: "coworker",
				content: "another reply",
				channel: "web",
				thread_parent_id: parentId,
				timestamp: "2026-01-01T00:00:00Z",
			},
		});

		const tracked = get(trackedThreads);
		expect(tracked[parentId]).toBeUndefined();
	});

	it("auto-tracks when parent from matches userSenderName (custom display name)", () => {
		userSenderName.set("ben");
		messagesByChannel.set({
			web: [{ id: "ben-msg", from: "ben", content: "custom name question", channel: "web" } as Message],
		});

		handleUpdate({
			type: "channel_message",
			data: {
				id: "reply-5",
				from: "coworker",
				content: "reply to ben",
				channel: "web",
				thread_parent_id: "ben-msg",
				timestamp: "2026-01-01T00:00:00Z",
			},
		});

		const tracked = get(trackedThreads);
		expect(tracked["ben-msg"]).toBeTruthy();
		expect(tracked["ben-msg"].subject).toContain("custom name question");
	});
});

describe("forkThread / unforkThread", () => {
	// When no WebSocket is connected (ws === null, the default module state),
	// these functions should call onError instead of silently dropping the request.

	it("forkThread calls onError when WebSocket is not connected", () => {
		const onError = vi.fn();
		forkThread("thread-123", "web", onError);
		expect(onError).toHaveBeenCalledWith(expect.stringContaining("onnect"));
	});

	it("unforkThread calls onError when WebSocket is not connected", () => {
		const onError = vi.fn();
		unforkThread("thread-123", "web", onError);
		expect(onError).toHaveBeenCalledWith(expect.stringContaining("onnect"));
	});

	it("forkThread does not throw when no onError provided and WS disconnected", () => {
		expect(() => forkThread("thread-123", "web")).not.toThrow();
	});

	it("unforkThread does not throw when no onError provided and WS disconnected", () => {
		expect(() => unforkThread("thread-123", "web")).not.toThrow();
	});
});

describe("onNextError callback leak on success path", () => {
	it("caller can clear error callback on success to prevent stale firing", () => {
		// Simulates the fixed flow: forkThread registers an onError callback
		// via onNextError. When the fork succeeds (thread_ownership update arrives),
		// the caller clears the callback using the returned ID.
		const onError = vi.fn();
		const callbackId = onNextError(onError);

		// Simulate success: thread_ownership update arrives
		handleUpdate({
			type: "thread_ownership",
			data: { thread_parent_id: "thread-1", has_dedicated_session: true, owner: "web-discuss-ab12" },
		});

		// Caller clears the callback on success (this is what ThreadPanel now does)
		clearErrorCallback(callbackId);

		// An unrelated error arrives — the stale callback must NOT fire
		handleUpdate({
			type: "error",
			data: { message: "unrelated error from different operation" },
		});

		expect(onError).not.toHaveBeenCalled();
	});

	it("clearErrorCallback prevents a registered callback from firing on subsequent errors", () => {
		// Test the lower-level API: onNextError returns an ID,
		// clearErrorCallback removes it, so a subsequent error doesn't fire the callback.
		const onError = vi.fn();
		const id = onNextError(onError);

		// Simulate success — caller clears the callback
		clearErrorCallback(id);

		// Now an unrelated error arrives
		handleUpdate({
			type: "error",
			data: { message: "some error" },
		});

		expect(onError).not.toHaveBeenCalled();
	});

	it("error callback fires correctly when an actual error occurs (no success)", () => {
		const onError = vi.fn();
		onNextError(onError);

		// Error arrives before any success — callback should fire
		handleUpdate({
			type: "error",
			data: { message: "fork failed: no channel lead" },
		});

		expect(onError).toHaveBeenCalledWith("fork failed: no channel lead");
	});
});

describe("fetchChannelAgentsMd — AbortController cancellation", () => {
	let originalFetch: typeof globalThis.fetch;

	beforeEach(() => {
		originalFetch = globalThis.fetch;
	});

	afterEach(() => {
		globalThis.fetch = originalFetch;
	});

	it("aborts a previous in-flight request when a new one starts for the same channel", async () => {
		const abortSignals: AbortSignal[] = [];
		globalThis.fetch = vi.fn().mockImplementation((_url: string, opts?: RequestInit) => {
			if (opts?.signal) abortSignals.push(opts.signal);
			return new Promise((resolve) =>
				setTimeout(() => resolve({ ok: true, json: async () => ({ content: "md", source: "channel-local" }) }), 100),
			);
		});

		// Start two concurrent fetches for the same channel
		const first = fetchChannelAgentsMd("web");
		const second = fetchChannelAgentsMd("web");

		await Promise.allSettled([first, second]);

		// The first request's signal should have been aborted
		expect(abortSignals).toHaveLength(2);
		expect(abortSignals[0].aborted).toBe(true);
		expect(abortSignals[1].aborted).toBe(false);
	});

	it("does not abort requests for different channels", async () => {
		const abortSignals: Record<string, AbortSignal> = {};
		globalThis.fetch = vi.fn().mockImplementation((url: string, opts?: RequestInit) => {
			// Extract channel from URL path like /channels/web/agents-md
			const match = url.match(/channels\/([^/]+)\/agents-md/);
			const ch = match ? match[1] : "unknown";
			if (opts?.signal) abortSignals[ch] = opts.signal;
			return Promise.resolve({ ok: true, json: async () => ({ content: "", source: "none" }) });
		});

		await Promise.all([fetchChannelAgentsMd("web"), fetchChannelAgentsMd("ops")]);

		expect(abortSignals.web.aborted).toBe(false);
		expect(abortSignals.ops.aborted).toBe(false);
	});
});

describe("fetchChannelAgentsMd — error distinction", () => {
	let originalFetch: typeof globalThis.fetch;

	beforeEach(() => {
		originalFetch = globalThis.fetch;
	});

	afterEach(() => {
		globalThis.fetch = originalFetch;
	});

	it("returns error: null on successful fetch", async () => {
		globalThis.fetch = vi.fn().mockResolvedValue({
			ok: true,
			json: async () => ({ content: "# Instructions", source: "channel-local" }),
		});

		const result = await fetchChannelAgentsMd("web");

		expect(result?.content).toBe("# Instructions");
		expect(result?.source).toBe("channel-local");
		expect(result?.error).toBeNull();
	});

	it("returns error string on HTTP failure (non-200)", async () => {
		globalThis.fetch = vi.fn().mockResolvedValue({
			ok: false,
			status: 500,
		});

		const result = await fetchChannelAgentsMd("web");

		expect(result?.content).toBe("");
		expect(result?.source).toBe("none");
		expect(result?.error).toBeTruthy();
		expect(typeof result?.error).toBe("string");
	});

	it("returns error string on network failure", async () => {
		globalThis.fetch = vi.fn().mockRejectedValue(new Error("Network error"));

		const result = await fetchChannelAgentsMd("web");

		expect(result?.content).toBe("");
		expect(result?.source).toBe("none");
		expect(result?.error).toBeTruthy();
		expect(typeof result?.error).toBe("string");
	});

	it("returns null when fetch is aborted so caller can bail out", async () => {
		let callCount = 0;
		globalThis.fetch = vi.fn().mockImplementation((_url: string, opts?: RequestInit) => {
			callCount++;
			if (callCount === 1) {
				// First call: hang until aborted
				return new Promise((_resolve, reject) => {
					if (opts?.signal?.aborted) {
						const err = new Error("The operation was aborted.");
						err.name = "AbortError";
						reject(err);
						return;
					}
					opts?.signal?.addEventListener("abort", () => {
						const err = new Error("The operation was aborted.");
						err.name = "AbortError";
						reject(err);
					});
				});
			}
			// Second call: resolve normally
			return Promise.resolve({ ok: true, json: async () => ({ content: "", source: "none" }) });
		});

		// Start first request, then immediately start second (which aborts first)
		const first = fetchChannelAgentsMd("abort-test");
		const second = fetchChannelAgentsMd("abort-test");

		const [firstResult, secondResult] = await Promise.all([first, second]);

		// Aborted requests return null so the caller can bail out
		expect(firstResult).toBeNull();
		// Second request should succeed normally
		expect(secondResult).not.toBeNull();
		expect(secondResult?.error).toBeNull();
	});
});
