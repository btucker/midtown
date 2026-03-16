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

	it("returns tasks with empty children when no parent relationships", () => {
		const tasks = [makeTask(1, "Task A"), makeTask(2, "Task B")];
		const result = groupTasksByParent(tasks);
		expect(result).toEqual([
			{ task: tasks[0], children: [] },
			{ task: tasks[1], children: [] },
		]);
	});

	it("nests child under its parent", () => {
		const parent = makeTask(10, "Implement feature");
		const child = makeTask(11, "Review PR #42", "10");
		const result = groupTasksByParent([parent, child]);
		expect(result).toEqual([{ task: parent, children: [child] }]);
	});

	it("shows child as top-level when parent is not in list", () => {
		const child = makeTask(11, "Review PR #42", "10");
		const result = groupTasksByParent([child]);
		expect(result).toEqual([{ task: child, children: [] }]);
	});

	it("groups multiple children under same parent", () => {
		const parent = makeTask(10, "Implement feature");
		const child1 = makeTask(11, "Review PR", "10");
		const child2 = makeTask(12, "Address feedback", "10");
		const result = groupTasksByParent([parent, child1, child2]);
		expect(result).toEqual([{ task: parent, children: [child1, child2] }]);
	});

	it("handles mixed parent and standalone tasks", () => {
		const standalone = makeTask(5, "Standalone");
		const parent = makeTask(10, "Feature");
		const child = makeTask(11, "Review", "10");
		const result = groupTasksByParent([standalone, parent, child]);
		expect(result).toEqual([
			{ task: standalone, children: [] },
			{ task: parent, children: [child] },
		]);
	});

	it("preserves input order for children listed before parent", () => {
		const child = makeTask(11, "Review", "10");
		const parent = makeTask(10, "Feature");
		// Child appears before parent in input — child should nest under parent
		const result = groupTasksByParent([child, parent]);
		expect(result).toEqual([{ task: parent, children: [child] }]);
	});

	it("handles self-parent by treating as top-level", () => {
		const task = makeTask(5, "Self-referencing", "5");
		const result = groupTasksByParent([task]);
		expect(result).toEqual([{ task, children: [] }]);
	});

	it("handles circular parent refs without losing tasks", () => {
		const a = makeTask(1, "Task A", "2");
		const b = makeTask(2, "Task B", "1");
		const result = groupTasksByParent([a, b]);
		// Both should appear as top-level — neither can be properly nested
		expect(result).toHaveLength(2);
		expect(result.every((r) => r.children.length === 0)).toBe(true);
	});

	it("does not duplicate tasks in any scenario", () => {
		const parent = makeTask(10, "Feature");
		const child = makeTask(11, "Review", "10");
		const orphan = makeTask(12, "Orphan", "99");
		const selfRef = makeTask(13, "Self", "13");
		const result = groupTasksByParent([parent, child, orphan, selfRef]);
		const allIds = result.flatMap((r) => [r.task.id, ...r.children.map((c) => c.id)]);
		expect(allIds).toHaveLength(new Set(allIds).size);
		expect(allIds).toContain(10);
		expect(allIds).toContain(11);
		expect(allIds).toContain(12);
		expect(allIds).toContain(13);
	});
});
