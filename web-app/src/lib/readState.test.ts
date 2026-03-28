import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Mock localStorage before importing modules
const localStorageMap = new Map<string, string>();
globalThis.localStorage = {
	getItem: (key: string) => localStorageMap.get(key) ?? null,
	setItem: (key: string, value: string) => localStorageMap.set(key, value),
	removeItem: (key: string) => localStorageMap.delete(key),
	clear: () => localStorageMap.clear(),
	get length() {
		return localStorageMap.size;
	},
	key: (_index: number) => null,
} as Storage;

// Import after mock is in place — module is evaluated once, so localStorage
// initialisation tests use vi.resetModules() + re-import to get a fresh copy.
import { channelReadState, readStateLoaded, threadReadState, threadUnreadCounts, trackedThreads } from "./store.ts";

describe("read state localStorage persistence", () => {
	beforeEach(() => {
		vi.useFakeTimers();
	});

	afterEach(() => {
		vi.useRealTimers();
	});

	it("persists threadReadState changes to localStorage", () => {
		threadReadState.set({ "thread-2": "2026-03-27T12:00:00Z" });
		// The subscription uses debounced save — advance past debounce delay
		vi.advanceTimersByTime(500);

		const stored = localStorageMap.get("midtown_thread_read_state") ?? "";
		expect(stored).toBeTruthy();
		expect(JSON.parse(stored)).toEqual({ "thread-2": "2026-03-27T12:00:00Z" });
	});

	it("persists channelReadState changes to localStorage", () => {
		channelReadState.set({ web: "2026-03-27T09:00:00Z" });
		vi.advanceTimersByTime(500);

		const stored = localStorageMap.get("midtown_channel_read_state") ?? "";
		expect(stored).toBeTruthy();
		expect(JSON.parse(stored)).toEqual({ web: "2026-03-27T09:00:00Z" });
	});
});

describe("read state warm-start from localStorage", () => {
	// These tests need a fresh module evaluation to verify initialisation,
	// since store.ts reads localStorage at import time.

	afterEach(() => {
		localStorageMap.clear();
		vi.restoreAllMocks();
	});

	it("initializes threadReadState from localStorage on module load", async () => {
		vi.resetModules();
		const cachedState = { "thread-1": "2026-03-27T10:00:00Z" };
		localStorageMap.set("midtown_thread_read_state", JSON.stringify(cachedState));

		const mod = await import("./store.ts");
		expect(get(mod.threadReadState)).toEqual(cachedState);
	});

	it("initializes channelReadState from localStorage on module load", async () => {
		vi.resetModules();
		const cachedState = { web: "2026-03-27T09:00:00Z" };
		localStorageMap.set("midtown_channel_read_state", JSON.stringify(cachedState));

		const mod = await import("./store.ts");
		expect(get(mod.channelReadState)).toEqual(cachedState);
	});

	it("sets readStateLoaded to true when localStorage has cached read state", async () => {
		vi.resetModules();
		localStorageMap.set("midtown_thread_read_state", JSON.stringify({ t: "x" }));

		const mod = await import("./store.ts");
		expect(get(mod.readStateLoaded)).toBe(true);
	});

	it("sets readStateLoaded to false when localStorage has no cached read state", async () => {
		vi.resetModules();
		localStorageMap.clear();

		const mod = await import("./store.ts");
		expect(get(mod.readStateLoaded)).toBe(false);
	});

	it("sets readStateLoaded to false when localStorage has empty {} objects", async () => {
		vi.resetModules();
		localStorageMap.set("midtown_thread_read_state", JSON.stringify({}));
		localStorageMap.set("midtown_channel_read_state", JSON.stringify({}));

		const mod = await import("./store.ts");
		expect(get(mod.readStateLoaded)).toBe(false);
	});
});

describe("syncUnreadCounts with cached read state", () => {
	beforeEach(() => {
		// Ensure readStateLoaded is true so syncUnreadCounts actually runs
		readStateLoaded.set(true);
	});

	afterEach(() => {
		// Reset stores to avoid cross-test pollution
		trackedThreads.set({});
		threadReadState.set({});
		threadUnreadCounts.set({});
	});

	it("does not mark thread as unread when read timestamp >= lastActivity", () => {
		threadReadState.set({ "thread-1": "2026-03-27T12:00:00Z" });
		trackedThreads.set({
			"thread-1": {
				channelName: "web",
				subject: "Test thread",
				lastActivity: "2026-03-27T11:00:00Z",
				replyCount: 3,
			},
		});

		// Thread was read AFTER lastActivity — should NOT be unread
		expect(get(threadUnreadCounts)["thread-1"]).toBeUndefined();
	});

	it("marks thread as unread when read timestamp < lastActivity", () => {
		threadReadState.set({ "thread-1": "2026-03-27T10:00:00Z" });
		trackedThreads.set({
			"thread-1": {
				channelName: "web",
				subject: "Test thread",
				lastActivity: "2026-03-27T11:00:00Z",
				replyCount: 3,
			},
		});

		expect(get(threadUnreadCounts)["thread-1"]).toBe(1);
	});

	it("marks thread as unread when no read timestamp exists", () => {
		threadReadState.set({});
		trackedThreads.set({
			"thread-1": {
				channelName: "web",
				subject: "Test thread",
				lastActivity: "2026-03-27T11:00:00Z",
				replyCount: 3,
			},
		});

		expect(get(threadUnreadCounts)["thread-1"]).toBe(1);
	});
});

describe("readStateLoaded gates unread counts", () => {
	afterEach(() => {
		trackedThreads.set({});
		threadReadState.set({});
		threadUnreadCounts.set({});
		readStateLoaded.set(false);
	});

	it("suppresses unread counts when readStateLoaded is false", () => {
		readStateLoaded.set(false);
		threadReadState.set({});
		trackedThreads.set({
			"thread-1": {
				channelName: "web",
				subject: "Test thread",
				lastActivity: "2026-03-27T11:00:00Z",
				replyCount: 1,
			},
		});

		// Even though there's no read timestamp, badges should be suppressed
		expect(get(threadUnreadCounts)).toEqual({});
	});

	it("shows unread counts once readStateLoaded becomes true", () => {
		readStateLoaded.set(false);
		threadReadState.set({});
		trackedThreads.set({
			"thread-1": {
				channelName: "web",
				subject: "Test thread",
				lastActivity: "2026-03-27T11:00:00Z",
				replyCount: 1,
			},
		});

		expect(get(threadUnreadCounts)).toEqual({});

		// Server fetch completes — flag flips
		readStateLoaded.set(true);
		expect(get(threadUnreadCounts)["thread-1"]).toBe(1);
	});
});
