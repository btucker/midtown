import { beforeEach, describe, expect, test, vi } from "vitest";
import { updatePreviewUrl } from "./filePaste.ts";

describe("updatePreviewUrl", () => {
	let mockCreate: ReturnType<typeof vi.fn>;
	let mockRevoke: ReturnType<typeof vi.fn>;

	beforeEach(() => {
		let counter = 0;
		mockCreate = vi.fn(() => `blob:mock-${++counter}`);
		mockRevoke = vi.fn();
		globalThis.URL.createObjectURL = mockCreate;
		globalThis.URL.revokeObjectURL = mockRevoke;
	});

	test("creates blob URL for a file", () => {
		const file = new File([""], "test.png", { type: "image/png" });
		const url = updatePreviewUrl(null, file);
		expect(mockCreate).toHaveBeenCalledWith(file);
		expect(url).toBe("blob:mock-1");
	});

	test("revokes previous URL when creating new one", () => {
		const file = new File([""], "test.png", { type: "image/png" });
		updatePreviewUrl("blob:old-url", file);
		expect(mockRevoke).toHaveBeenCalledWith("blob:old-url");
		expect(mockCreate).toHaveBeenCalledWith(file);
	});

	test("returns null and revokes when file is null", () => {
		const url = updatePreviewUrl("blob:old-url", null);
		expect(mockRevoke).toHaveBeenCalledWith("blob:old-url");
		expect(url).toBeNull();
	});

	test("returns null without revoking when both are null", () => {
		const url = updatePreviewUrl(null, null);
		expect(mockRevoke).not.toHaveBeenCalled();
		expect(url).toBeNull();
	});
});
