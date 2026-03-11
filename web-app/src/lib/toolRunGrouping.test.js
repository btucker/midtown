import { describe, expect, it } from "vitest";
import { groupTimelineToolRuns, groupToolRuns, isToolOnly } from "./toolRunGrouping.js";

describe("isToolOnly", () => {
	it("returns true for empty content with tool_data", () => {
		expect(isToolOnly({ content: "", tool_data: [{ tool_name: "Bash" }] })).toBe(true);
	});

	it("returns true for null content with tool_data", () => {
		expect(isToolOnly({ content: null, tool_data: [{ tool_name: "Read" }] })).toBe(true);
	});

	it("returns true for undefined content with tool_data", () => {
		expect(isToolOnly({ content: undefined, tool_data: [{ tool_name: "Read" }] })).toBe(true);
	});

	it("returns true for whitespace-only content with tool_data", () => {
		expect(isToolOnly({ content: "   \n\t  ", tool_data: [{ tool_name: "Bash" }] })).toBe(true);
	});

	it("returns false when content has text", () => {
		expect(isToolOnly({ content: "Here are the results:", tool_data: [{ tool_name: "Bash" }] })).toBe(false);
	});

	it("returns false for empty tool_data array", () => {
		expect(isToolOnly({ content: "", tool_data: [] })).toBe(false);
	});

	it("returns false for null tool_data", () => {
		expect(isToolOnly({ content: "", tool_data: null })).toBe(false);
	});

	it("returns false for undefined tool_data", () => {
		expect(isToolOnly({ content: "" })).toBe(false);
	});

	it("returns false for plain text message", () => {
		expect(isToolOnly({ content: "Hello world" })).toBe(false);
	});
});

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

// Helpers for timeline entries
function timelineMsg(id, content, toolData = null) {
	return {
		type: "message",
		data: {
			id,
			content,
			tool_data: toolData,
			timestamp: new Date().toISOString(),
		},
		timestamp: new Date().toISOString(),
		msgIndex: 0,
	};
}

function timelineToolMsg(id, tools) {
	return timelineMsg(
		id,
		"",
		tools.map((name) => ({ tool_name: name, input: {} })),
	);
}

function timelineEdit(id) {
	return {
		type: "edit",
		data: {
			itemId: id,
			filePath: "test.js",
			oldString: "a",
			newString: "b",
			timestamp: new Date().toISOString(),
		},
		timestamp: new Date().toISOString(),
		msgIndex: -1,
	};
}

describe("groupTimelineToolRuns", () => {
	it("returns empty array for empty input", () => {
		expect(groupTimelineToolRuns([])).toEqual([]);
	});

	it("groups consecutive tool-only timeline entries", () => {
		const entries = [timelineToolMsg("1", ["Bash"]), timelineToolMsg("2", ["Read", "Grep"])];
		const groups = groupTimelineToolRuns(entries);
		expect(groups).toHaveLength(1);
		expect(groups[0].type).toBe("tool-run");
		expect(groups[0].toolCount).toBe(3);
		expect(groups[0].entries).toEqual(entries);
	});

	it("does not group a single tool-only entry", () => {
		const entries = [timelineToolMsg("1", ["Bash"])];
		const groups = groupTimelineToolRuns(entries);
		expect(groups).toHaveLength(1);
		expect(groups[0].type).toBe("message");
	});

	it("edit entries break tool runs", () => {
		const entries = [timelineToolMsg("1", ["Bash"]), timelineEdit("e1"), timelineToolMsg("2", ["Read"])];
		const groups = groupTimelineToolRuns(entries);
		expect(groups).toHaveLength(3);
		expect(groups[0].type).toBe("message"); // single tool, not grouped
		expect(groups[1].type).toBe("edit");
		expect(groups[2].type).toBe("message"); // single tool, not grouped
	});

	it("text messages break tool runs", () => {
		const entries = [
			timelineToolMsg("1", ["Bash"]),
			timelineToolMsg("2", ["Read"]),
			timelineMsg("3", "Done!", null),
			timelineToolMsg("4", ["Edit"]),
			timelineToolMsg("5", ["Bash"]),
		];
		const groups = groupTimelineToolRuns(entries);
		expect(groups).toHaveLength(3);
		expect(groups[0].type).toBe("tool-run");
		expect(groups[0].toolCount).toBe(2);
		expect(groups[1].type).toBe("message");
		expect(groups[1].data.id).toBe("3");
		expect(groups[2].type).toBe("tool-run");
		expect(groups[2].toolCount).toBe(2);
	});
});
