import { getSenderColor } from "./messageUtils.ts";
import type { Task } from "./types.ts";

/** Compute the rolled-up status for a parent task with children. */
export function rolledUpStatus(task: Task, children: Task[], isCard: boolean): string {
	if (isCard || children.length === 0) return task.status;
	const all = [task, ...children];
	if (all.some((t) => t.status === "in_progress")) return "in_progress";
	if (all.every((t) => t.status === "completed")) return "completed";
	return task.status;
}

/** Status bar color for a task row. */
export function statusBarColor(status: string, owner: string | null, colorOverride: string | null): string {
	if (status === "completed") return "hsl(var(--accent-green, 145 40% 38%))";
	if (status !== "in_progress") return "hsl(var(--muted-foreground) / 0.3)";
	if (colorOverride) return colorOverride;
	if (owner) return getSenderColor(owner);
	return "hsl(var(--accent-teal))";
}
