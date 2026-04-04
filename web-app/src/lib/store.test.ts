import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Mock localStorage and window.addEventListener for Node test environment
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

// Capture beforeunload handlers registered during module import
const beforeUnloadHandlers: ((event: BeforeUnloadEvent) => void)[] = [];
globalThis.window = globalThis.window || ({} as Window & typeof globalThis);
const origAddEventListener = globalThis.window.addEventListener;
// biome-ignore lint/suspicious/noExplicitAny: test mock override
(globalThis.window as any).addEventListener = (event: string, handler: EventListenerOrEventListenerObject) => {
	if (event === "beforeunload") beforeUnloadHandlers.push(handler as (event: BeforeUnloadEvent) => void);
	if (origAddEventListener)
		(origAddEventListener as (...args: [string, EventListenerOrEventListenerObject]) => void).call(
			globalThis.window,
			event,
			handler,
		);
};

// Import after mocks are in place
const { debouncedSaveToLocalStorage, flushDebouncedSaves } = await import("./store.ts");

describe("debouncedSaveToLocalStorage", () => {
	beforeEach(() => {
		vi.useFakeTimers();
		localStorageMap.clear();
	});

	afterEach(() => {
		vi.useRealTimers();
	});

	it("does not write to localStorage immediately", () => {
		debouncedSaveToLocalStorage("test_key", { count: 1 });
		expect(localStorage.getItem("test_key")).toBeNull();
	});

	it("writes to localStorage after the debounce delay", () => {
		debouncedSaveToLocalStorage("test_key", { count: 1 });
		vi.advanceTimersByTime(500);
		expect(JSON.parse(localStorage.getItem("test_key") ?? "")).toEqual({ count: 1 });
	});

	it("coalesces rapid writes — only the last value is persisted", () => {
		debouncedSaveToLocalStorage("test_key", { count: 1 });
		debouncedSaveToLocalStorage("test_key", { count: 2 });
		debouncedSaveToLocalStorage("test_key", { count: 3 });

		vi.advanceTimersByTime(500);

		expect(JSON.parse(localStorage.getItem("test_key") ?? "")).toEqual({ count: 3 });
	});

	it("handles different keys independently", () => {
		debouncedSaveToLocalStorage("key_a", "value_a");
		debouncedSaveToLocalStorage("key_b", "value_b");

		vi.advanceTimersByTime(500);

		expect(JSON.parse(localStorage.getItem("key_a")!)).toBe("value_a");
		expect(JSON.parse(localStorage.getItem("key_b")!)).toBe("value_b");
	});

	it("flushDebouncedSaves writes all pending values immediately", () => {
		debouncedSaveToLocalStorage("flush_key", { flushed: true });
		flushDebouncedSaves();

		expect(JSON.parse(localStorage.getItem("flush_key")!)).toEqual({ flushed: true });
	});

	it("registers a beforeunload handler that flushes pending writes", () => {
		expect(beforeUnloadHandlers.length).toBeGreaterThan(0);

		debouncedSaveToLocalStorage("unload_key", { saved: true });

		// Simulate tab close by calling the registered beforeunload handler
		beforeUnloadHandlers.forEach((handler) => handler({} as BeforeUnloadEvent));

		expect(JSON.parse(localStorage.getItem("unload_key")!)).toEqual({ saved: true });
	});
});
