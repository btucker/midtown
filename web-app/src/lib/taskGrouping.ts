import type { Task } from "./types.ts";

export interface GroupedTask {
	task: Task;
	depth: number;
}

/**
 * Arrange tasks into parent-child groups for display.
 *
 * Returns a flat list with depth annotations:
 * - Top-level tasks (no parent, or parent not in the list) get depth 0
 * - Children are placed immediately after their parent with depth 1
 *
 * Children whose parent is not in the visible list are shown at depth 0
 * to avoid orphaned invisible tasks.
 */
export function groupTasksByParent(tasks: Task[]): GroupedTask[] {
	const taskIds = new Set(tasks.map((t) => String(t.id)));
	const topLevelIds = new Set<string>();

	// Separate into parents/top-level and children-with-visible-parent.
	// A task is only treated as a child if its parent is in the list AND
	// is not itself a child (prevents cycles and limits nesting to 1 level).
	const childrenByParent = new Map<string, Task[]>();
	const topLevel: Task[] = [];

	// First pass: identify which tasks are top-level (no parent or parent not in list)
	for (const task of tasks) {
		const parentId = task.parent;
		if (!parentId || !taskIds.has(parentId) || parentId === String(task.id)) {
			topLevel.push(task);
			topLevelIds.add(String(task.id));
		}
	}

	// Second pass: classify remaining tasks as children only if their parent is top-level
	for (const task of tasks) {
		const parentId = task.parent;
		if (parentId && parentId !== String(task.id) && taskIds.has(parentId)) {
			if (topLevelIds.has(parentId)) {
				const siblings = childrenByParent.get(parentId) || [];
				siblings.push(task);
				childrenByParent.set(parentId, siblings);
			} else {
				// Parent is itself a child or in a cycle — promote to top level
				topLevel.push(task);
			}
		}
	}

	// Build flat list: each top-level task followed by its children
	const result: GroupedTask[] = [];
	for (const task of topLevel) {
		result.push({ task, depth: 0 });
		const children = childrenByParent.get(String(task.id));
		if (children) {
			for (const child of children) {
				result.push({ task: child, depth: 1 });
			}
		}
	}

	return result;
}
