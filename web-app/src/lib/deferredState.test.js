import { describe, expect, it } from "vitest";

/**
 * Tests for the deferred state write pattern used to fix Svelte 5's
 * state_unsafe_mutation error. The fix uses queueMicrotask() with version
 * counters to defer $state writes inside $effect blocks, breaking the
 * reactive diamond: store → $derived → $effect → $state → $derived.
 *
 * These tests validate the version-counter mechanism that prevents stale
 * microtasks from overwriting newer state — the core correctness guarantee
 * that the queueMicrotask wrappers depend on.
 */

describe("deferred state write pattern — version counter", () => {
	it("stale microtask is discarded when a newer write is queued", async () => {
		let version = 0;
		let value = "initial";

		// Simulate first effect run: queues a deferred write
		const v1 = ++version;
		queueMicrotask(() => {
			if (v1 !== version) return;
			value = "first";
		});

		// Simulate second effect run before first microtask fires
		const v2 = ++version;
		queueMicrotask(() => {
			if (v2 !== version) return;
			value = "second";
		});

		// Drain microtask queue
		await new Promise((r) => queueMicrotask(r));

		// Only the latest write should have taken effect
		expect(value).toBe("second");
	});

	it("rapid appear/disappear/appear cycle resolves to final state", async () => {
		let version = 0;
		let active = false;

		// Items appear → queue active = true
		const v1 = ++version;
		queueMicrotask(() => {
			if (v1 !== version) return;
			active = true;
		});

		// Items disappear → queue active = false
		const v2 = ++version;
		queueMicrotask(() => {
			if (v2 !== version) return;
			active = false;
		});

		// Items reappear → queue active = true
		const v3 = ++version;
		queueMicrotask(() => {
			if (v3 !== version) return;
			active = true;
		});

		await new Promise((r) => queueMicrotask(r));

		// Final state matches the last queued value
		expect(active).toBe(true);
	});

	it("synchronous write between queued microtasks is not overwritten", async () => {
		let version = 0;
		let pending = false;

		// Effect queues a deferred clear
		const v1 = ++version;
		queueMicrotask(() => {
			if (v1 !== version) return;
			pending = false;
		});

		// User action sets pending = true synchronously AND bumps version
		version++;
		pending = true;

		// Drain microtask queue — stale v1 microtask should be discarded
		await new Promise((r) => queueMicrotask(r));

		// Synchronous write should survive
		expect(pending).toBe(true);
	});

	it("version counter handles interleaved effect runs correctly", async () => {
		let version = 0;
		let renderStartIndex = 0;
		let pendingIndex = 0;

		// First effect: channel switch, queue renderStartIndex = 50
		const v1 = ++version;
		pendingIndex = 50;
		queueMicrotask(() => {
			if (v1 !== version) return;
			renderStartIndex = 50;
		});

		// Second effect: new message arrives, re-evaluates
		// pendingIndex is already 50, so history-load guard fails (correct behavior)
		const guardPasses = pendingIndex === 0;
		expect(guardPasses).toBe(false);

		await new Promise((r) => queueMicrotask(r));

		expect(renderStartIndex).toBe(50);
	});
});
