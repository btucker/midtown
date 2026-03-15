import { describe, expect, it } from "vitest";
import { groupTasksByParent } from "./taskGrouping.ts";
import type { Task } from "./types.ts";

function makeTask(id: number, subject: string, parent?: string): Task {
	return { id, subject, status: "in_progress", parent };
}

describe("groupTasksByParent", () => {
	it("returns empty array for empty input", () => {
		expect(groupTasksByParent([])).toEqual([]);
	});

	it("returns tasks at depth 0 when no parent relationships", () => {
		const tasks = [makeTask(1, "Task A"), makeTask(2, "Task B")];
		const result = groupTasksByParent(tasks);
		expect(result).toEqual([
			{ task: tasks[0], depth: 0 },
			{ task: tasks[1], depth: 0 },
		]);
	});

	it("nests child under its parent at depth 1", () => {
		const parent = makeTask(10, "Implement feature");
		const child = makeTask(11, "Review PR #42", "10");
		const result = groupTasksByParent([parent, child]);
		expect(result).toEqual([
			{ task: parent, depth: 0 },
			{ task: child, depth: 1 },
		]);
	});

	it("shows child at depth 0 when parent is not in list", () => {
		const child = makeTask(11, "Review PR #42", "10");
		const result = groupTasksByParent([child]);
		expect(result).toEqual([{ task: child, depth: 0 }]);
	});

	it("groups multiple children under same parent", () => {
		const parent = makeTask(10, "Implement feature");
		const child1 = makeTask(11, "Review PR", "10");
		const child2 = makeTask(12, "Address feedback", "10");
		const result = groupTasksByParent([parent, child1, child2]);
		expect(result).toEqual([
			{ task: parent, depth: 0 },
			{ task: child1, depth: 1 },
			{ task: child2, depth: 1 },
		]);
	});

	it("handles mixed parent and standalone tasks", () => {
		const standalone = makeTask(5, "Standalone");
		const parent = makeTask(10, "Feature");
		const child = makeTask(11, "Review", "10");
		const result = groupTasksByParent([standalone, parent, child]);
		expect(result).toEqual([
			{ task: standalone, depth: 0 },
			{ task: parent, depth: 0 },
			{ task: child, depth: 1 },
		]);
	});

	it("preserves input order for children listed before parent", () => {
		const child = makeTask(11, "Review", "10");
		const parent = makeTask(10, "Feature");
		// Child appears before parent in input — child should nest under parent
		const result = groupTasksByParent([child, parent]);
		expect(result).toEqual([
			{ task: parent, depth: 0 },
			{ task: child, depth: 1 },
		]);
	});

	it("handles self-parent by treating as top-level", () => {
		const task = makeTask(5, "Self-referencing", "5");
		const result = groupTasksByParent([task]);
		expect(result).toEqual([{ task, depth: 0 }]);
	});

	it("handles circular parent refs without losing tasks", () => {
		const a = makeTask(1, "Task A", "2");
		const b = makeTask(2, "Task B", "1");
		const result = groupTasksByParent([a, b]);
		// Both should appear — neither can be properly nested
		expect(result).toHaveLength(2);
		expect(result.every((r) => r.depth === 0)).toBe(true);
	});

	it("does not duplicate tasks in any scenario", () => {
		const parent = makeTask(10, "Feature");
		const child = makeTask(11, "Review", "10");
		const orphan = makeTask(12, "Orphan", "99");
		const selfRef = makeTask(13, "Self", "13");
		const result = groupTasksByParent([parent, child, orphan, selfRef]);
		const ids = result.map((r) => r.task.id);
		expect(ids).toHaveLength(new Set(ids).size);
		expect(ids).toContain(10);
		expect(ids).toContain(11);
		expect(ids).toContain(12);
		expect(ids).toContain(13);
	});
});
