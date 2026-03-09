import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { computeInitialState, createAutoCollapse } from "./useAutoCollapse.js";

describe("computeInitialState", () => {
	it("returns 'collapsed' when message is older than 30s", () => {
		const old = new Date(Date.now() - 60_000).toISOString();
		expect(computeInitialState(old)).toBe("collapsed");
	});

	it("returns 'preview' when message is newer than 30s", () => {
		const recent = new Date(Date.now() - 5_000).toISOString();
		expect(computeInitialState(recent)).toBe("preview");
	});

	it("returns 'collapsed' when timestamp is missing", () => {
		expect(computeInitialState(null)).toBe("collapsed");
		expect(computeInitialState(undefined)).toBe("collapsed");
	});

	it("returns 'preview' when timestamp is exactly now", () => {
		const now = new Date().toISOString();
		expect(computeInitialState(now)).toBe("preview");
	});
});

describe("computeInitialState with custom delay", () => {
	it("uses custom delay when provided", () => {
		const ts = new Date(Date.now() - 45_000).toISOString();
		expect(computeInitialState(ts)).toBe("collapsed");
		expect(computeInitialState(ts, Date.now(), 60_000)).toBe("preview");
	});

	it("collapses at custom delay boundary", () => {
		const ts = new Date(Date.now() - 60_000).toISOString();
		expect(computeInitialState(ts, Date.now(), 60_000)).toBe("collapsed");
	});
});

describe("createAutoCollapse", () => {
	beforeEach(() => {
		vi.useFakeTimers();
	});
	afterEach(() => {
		vi.useRealTimers();
	});

	it("returns collapsed with null timeoutMs for old messages", () => {
		const old = new Date(Date.now() - 60_000).toISOString();
		const ac = createAutoCollapse(old);
		expect(ac.initial).toBe("collapsed");
		expect(ac.timeoutMs).toBeNull();
	});

	it("returns preview with positive timeoutMs for recent messages", () => {
		const recent = new Date(Date.now() - 5_000).toISOString();
		const ac = createAutoCollapse(recent);
		expect(ac.initial).toBe("preview");
		expect(ac.timeoutMs).toBeGreaterThan(0);
		expect(ac.timeoutMs).toBeLessThanOrEqual(25_000);
	});

	it("startTimer fires callback after timeout", () => {
		const recent = new Date(Date.now() - 5_000).toISOString();
		const ac = createAutoCollapse(recent);
		const cb = vi.fn();
		ac.startTimer(cb);
		expect(cb).not.toHaveBeenCalled();
		vi.advanceTimersByTime(ac.timeoutMs);
		expect(cb).toHaveBeenCalledOnce();
	});

	it("clearTimer prevents callback from firing", () => {
		const recent = new Date(Date.now() - 5_000).toISOString();
		const ac = createAutoCollapse(recent);
		const cb = vi.fn();
		ac.startTimer(cb);
		ac.clearTimer();
		vi.advanceTimersByTime(30_000);
		expect(cb).not.toHaveBeenCalled();
	});

	it("startTimer is a no-op when already collapsed", () => {
		const old = new Date(Date.now() - 60_000).toISOString();
		const ac = createAutoCollapse(old);
		const cb = vi.fn();
		ac.startTimer(cb);
		vi.advanceTimersByTime(60_000);
		expect(cb).not.toHaveBeenCalled();
	});
});
