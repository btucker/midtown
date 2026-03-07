import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { debouncedSaveToLocalStorage, flushDebouncedSaves } from "./store.js";

// Mock localStorage for Node test environment
const localStorageMap = new Map();
globalThis.localStorage = {
	getItem: (key) => localStorageMap.get(key) ?? null,
	setItem: (key, value) => localStorageMap.set(key, value),
	removeItem: (key) => localStorageMap.delete(key),
	clear: () => localStorageMap.clear(),
};

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
		expect(JSON.parse(localStorage.getItem("test_key"))).toEqual({ count: 1 });
	});

	it("coalesces rapid writes — only the last value is persisted", () => {
		debouncedSaveToLocalStorage("test_key", { count: 1 });
		debouncedSaveToLocalStorage("test_key", { count: 2 });
		debouncedSaveToLocalStorage("test_key", { count: 3 });

		vi.advanceTimersByTime(500);

		expect(JSON.parse(localStorage.getItem("test_key"))).toEqual({ count: 3 });
	});

	it("handles different keys independently", () => {
		debouncedSaveToLocalStorage("key_a", "value_a");
		debouncedSaveToLocalStorage("key_b", "value_b");

		vi.advanceTimersByTime(500);

		expect(JSON.parse(localStorage.getItem("key_a"))).toBe("value_a");
		expect(JSON.parse(localStorage.getItem("key_b"))).toBe("value_b");
	});

	it("flushDebouncedSaves writes all pending values immediately", () => {
		debouncedSaveToLocalStorage("flush_key", { flushed: true });
		flushDebouncedSaves();

		expect(JSON.parse(localStorage.getItem("flush_key"))).toEqual({ flushed: true });
	});
});
