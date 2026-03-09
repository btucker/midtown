import { describe, expect, it } from "vitest";
import { groupToolRuns } from "./toolRunGrouping.js";

function msg(id, content, toolData = null) {
	return { id, content, tool_data: toolData, timestamp: new Date().toISOString() };
}

function toolMsg(id, tools) {
	return msg(
		id,
		"",
		tools.map((name) => ({ tool_name: name, input: {} })),
	);
}

describe("groupToolRuns", () => {
	it("returns empty array for empty input", () => {
		expect(groupToolRuns([])).toEqual([]);
	});

	it("does not group messages with text content", () => {
		const msgs = [msg("1", "hello"), msg("2", "world")];
		const groups = groupToolRuns(msgs);
		expect(groups).toEqual([
			{ type: "message", message: msgs[0] },
			{ type: "message", message: msgs[1] },
		]);
	});

	it("groups consecutive tool-only messages into a run", () => {
		const msgs = [toolMsg("1", ["Bash"]), toolMsg("2", ["Read", "Grep"]), toolMsg("3", ["Edit"])];
		const groups = groupToolRuns(msgs);
		expect(groups).toHaveLength(1);
		expect(groups[0].type).toBe("tool-run");
		expect(groups[0].messages).toEqual(msgs);
		expect(groups[0].toolCount).toBe(4);
	});

	it("does not create a run from a single tool message", () => {
		const msgs = [toolMsg("1", ["Bash"])];
		const groups = groupToolRuns(msgs);
		expect(groups).toEqual([{ type: "message", message: msgs[0] }]);
	});

	it("breaks runs on text messages", () => {
		const msgs = [
			toolMsg("1", ["Bash"]),
			toolMsg("2", ["Read"]),
			msg("3", "Done!"),
			toolMsg("4", ["Edit"]),
			toolMsg("5", ["Bash"]),
		];
		const groups = groupToolRuns(msgs);
		expect(groups).toHaveLength(3);
		expect(groups[0].type).toBe("tool-run");
		expect(groups[0].toolCount).toBe(2);
		expect(groups[1].type).toBe("message");
		expect(groups[1].message.id).toBe("3");
		expect(groups[2].type).toBe("tool-run");
		expect(groups[2].toolCount).toBe(2);
	});

	it("treats whitespace-only content as tool-only", () => {
		const msgs = [msg("1", "  ", [{ tool_name: "Bash", input: {} }]), toolMsg("2", ["Read"])];
		const groups = groupToolRuns(msgs);
		expect(groups).toHaveLength(1);
		expect(groups[0].type).toBe("tool-run");
	});

	it("uses timestamp of last message in run", () => {
		const msgs = [
			{ ...toolMsg("1", ["Bash"]), timestamp: "2026-01-01T00:00:00Z" },
			{ ...toolMsg("2", ["Read"]), timestamp: "2026-01-01T00:01:00Z" },
		];
		const groups = groupToolRuns(msgs);
		expect(groups[0].lastTimestamp).toBe("2026-01-01T00:01:00Z");
	});
});
