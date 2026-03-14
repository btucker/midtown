import { beforeEach, describe, expect, it, vi } from "vitest";

// We test getSelkie by importing a fresh module for each test,
// since the module caches state in top-level variables.

async function loadFreshModule() {
	// Reset module registry so each test gets fresh state
	vi.resetModules();
	return import("./selkie.ts");
}

describe("getSelkie", () => {
	beforeEach(() => {
		vi.resetModules();
		vi.restoreAllMocks();
	});

	it("returns a renderer with a render method on successful init", async () => {
		const mockSvg = "<svg>test</svg>";
		vi.doMock("mermaid", () => ({
			default: {
				initialize: vi.fn(),
				render: vi.fn().mockResolvedValue({ svg: mockSvg }),
			},
		}));

		const { getSelkie } = await loadFreshModule();
		const result = await getSelkie();
		expect(result).toHaveProperty("render");
		const rendered = await result.render("test-id", "graph TD\n  A-->B");
		expect(rendered.svg).toBe(mockSvg);
	});

	it("caches the module after successful init", async () => {
		const mockInitialize = vi.fn();
		vi.doMock("mermaid", () => ({
			default: {
				initialize: mockInitialize,
				render: vi.fn().mockResolvedValue({ svg: "" }),
			},
		}));

		const { getSelkie } = await loadFreshModule();
		await getSelkie();
		await getSelkie();
		// initialize should only be called once
		expect(mockInitialize).toHaveBeenCalledTimes(1);
	});

	it("retries initialization after a failure", async () => {
		let callCount = 0;
		vi.doMock("mermaid", () => ({
			default: {
				initialize: vi.fn().mockImplementation(() => {
					callCount++;
					if (callCount === 1) throw new Error("Init failed");
				}),
				render: vi.fn().mockResolvedValue({ svg: "" }),
			},
		}));

		const { getSelkie } = await loadFreshModule();

		// First call should fail
		await expect(getSelkie()).rejects.toThrow("Init failed");

		// Second call should retry and succeed
		const result = await getSelkie();
		expect(result).toHaveProperty("render");
	});
});
