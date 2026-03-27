import { describe, expect, it } from "vitest";
import { rolledUpStatus, statusBarColor } from "./taskStatus.ts";
import type { Task } from "./types.ts";

function makeTask(status: string): Task {
	return { id: 1, subject: "Test", status };
}

describe("rolledUpStatus", () => {
	it("returns 'completed' when parent and all children are completed", () => {
		const parent = makeTask("completed");
		const children = [makeTask("completed"), makeTask("completed")];
		expect(rolledUpStatus(parent, children, false)).toBe("completed");
	});

	it("returns 'in_progress' when any task is in_progress", () => {
		const parent = makeTask("completed");
		const children = [makeTask("in_progress"), makeTask("completed")];
		expect(rolledUpStatus(parent, children, false)).toBe("in_progress");
	});

	it("returns parent status when children have mixed non-active statuses", () => {
		const parent = makeTask("in_progress");
		const children = [makeTask("completed"), makeTask("pending")];
		expect(rolledUpStatus(parent, children, false)).toBe("in_progress");
	});

	it("returns task.status directly when isCard is true", () => {
		const parent = makeTask("completed");
		expect(rolledUpStatus(parent, [makeTask("in_progress")], true)).toBe("completed");
	});

	it("returns task.status directly when there are no children", () => {
		const parent = makeTask("pending");
		expect(rolledUpStatus(parent, [], false)).toBe("pending");
	});
});

describe("statusBarColor", () => {
	it("returns green for completed status", () => {
		expect(statusBarColor("completed", null, null)).toBe("hsl(var(--accent-green, 145 40% 38%))");
	});

	it("returns muted for non-active, non-completed status", () => {
		expect(statusBarColor("pending", null, null)).toBe("hsl(var(--muted-foreground) / 0.3)");
	});

	it("returns color override for in_progress with override", () => {
		expect(statusBarColor("in_progress", "alice", "red")).toBe("red");
	});

	it("returns owner color for in_progress with owner", () => {
		const result = statusBarColor("in_progress", "alice", null);
		expect(result).toBeTruthy();
		expect(result).not.toBe("hsl(var(--accent-teal))");
	});

	it("returns teal for in_progress with no owner", () => {
		expect(statusBarColor("in_progress", null, null)).toBe("hsl(var(--accent-teal))");
	});
});
