import { describe, expect, it } from "vitest";
import { computeInitialState } from "./useAutoCollapse.js";

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
