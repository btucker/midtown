import { describe, expect, it, vi } from "vitest";
import { getForkOwnerColor } from "./avenue-colors.ts";
import { AVENUE_COLORS, dateChanged, getPermalinkUrl, getSenderColor, senderChanged } from "./messageUtils.ts";
import type { Message } from "./types.ts";

describe("getSenderColor", () => {
	it("returns gold for sender matching channel name (channel lead rule)", () => {
		expect(getSenderColor("web", undefined, "web")).toBe(AVENUE_COLORS.lead);
	});

	it("returns gold for midtown sender in midtown channel via channel lead rule", () => {
		expect(getSenderColor("midtown", undefined, "midtown")).toBe(AVENUE_COLORS.lead);
	});

	it("returns gold for midtown sender even without channelName (AVENUE_COLORS fallback)", () => {
		expect(getSenderColor("midtown", undefined)).toBe(AVENUE_COLORS.lead);
	});

	it("does not color a non-lead sender gold when channelName is provided", () => {
		expect(getSenderColor("lexington", undefined, "web")).toBe(AVENUE_COLORS.lexington);
	});

	it("is case-insensitive for channel name matching", () => {
		expect(getSenderColor("Web", undefined, "web")).toBe(AVENUE_COLORS.lead);
		expect(getSenderColor("web", undefined, "Web")).toBe(AVENUE_COLORS.lead);
	});

	it("returns fallback gray for unknown sender with no channel match", () => {
		expect(getSenderColor("unknown", undefined, "web")).toBe("#d0d0d0");
	});

	it("respects overrides before AVENUE_COLORS lookup", () => {
		const overrides = { lexington: "#ff0000" };
		expect(getSenderColor("lexington", overrides, "web")).toBe("#ff0000");
	});

	it("channel lead rule takes priority over overrides", () => {
		// Even if an override exists for 'web', sender=channel → gold wins
		const overrides = { web: "#ff0000" };
		expect(getSenderColor("web", overrides, "web")).toBe(AVENUE_COLORS.lead);
	});
});

describe("dateChanged", () => {
	function msg(timestamp: string): Message {
		return { timestamp, from: "test", content: "hello", id: "" } as Message;
	}

	it("returns null for the first message (index 0)", () => {
		const msgs = [msg("2026-03-02T10:00:00Z")];
		expect(dateChanged(msgs, 0)).toBe(null);
	});

	it("returns null when messages are on the same day and within 8 hours", () => {
		const msgs = [msg("2026-03-02T10:00:00Z"), msg("2026-03-02T14:00:00Z")];
		expect(dateChanged(msgs, 1)).toBe(null);
	});

	it("returns a date label when the calendar date changes", () => {
		vi.useFakeTimers();
		vi.setSystemTime(new Date("2026-03-02T20:00:00Z"));
		const msgs = [msg("2026-03-01T12:00:00Z"), msg("2026-03-02T12:00:00Z")];
		expect(dateChanged(msgs, 1)).toBe("Today");
		vi.useRealTimers();
	});

	it('returns "Yesterday" for the previous day', () => {
		vi.useFakeTimers();
		vi.setSystemTime(new Date("2026-03-03T12:00:00Z"));
		const msgs = [msg("2026-03-01T23:00:00Z"), msg("2026-03-02T10:00:00Z")];
		expect(dateChanged(msgs, 1)).toBe("Yesterday");
		vi.useRealTimers();
	});

	it("returns full date for older messages", () => {
		vi.useFakeTimers();
		vi.setSystemTime(new Date("2026-03-10T12:00:00Z"));
		const msgs = [msg("2026-02-27T23:00:00Z"), msg("2026-02-28T10:00:00Z")];
		const result = dateChanged(msgs, 1);
		expect(result).toContain("February");
		expect(result).toContain("28");
		expect(result).toContain("2026");
		vi.useRealTimers();
	});

	it("returns a date label for 8+ hour gap on the same day", () => {
		vi.useFakeTimers();
		vi.setSystemTime(new Date("2026-03-02T22:00:00Z"));
		const msgs = [
			msg("2026-03-02T02:00:00Z"),
			msg("2026-03-02T10:00:01Z"), // 8 hours + 1 second gap
		];
		expect(dateChanged(msgs, 1)).toBe("Today");
		vi.useRealTimers();
	});

	it("returns null for gap just under 8 hours on the same day", () => {
		const msgs = [
			msg("2026-03-02T14:00:00Z"),
			msg("2026-03-02T21:59:59Z"), // 7h 59m 59s gap
		];
		expect(dateChanged(msgs, 1)).toBe(null);
	});

	it("returns null for invalid timestamps", () => {
		const msgs = [msg("invalid"), msg("2026-03-02T10:00:00Z")];
		expect(dateChanged(msgs, 1)).toBe(null);
	});
});

describe("getPermalinkUrl", () => {
	it("generates thread-level URL for channel messages (no threadParentId)", () => {
		expect(getPermalinkUrl("myproject", "web", "msg-123")).toBe("/myproject?channel=web&thread=msg-123");
	});

	it("generates message-level URL for thread replies (with threadParentId)", () => {
		expect(getPermalinkUrl("myproject", "web", "reply-456", "parent-123")).toBe(
			"/myproject?channel=web&thread=parent-123&msg=reply-456",
		);
	});

	it("encodes special characters in URL components", () => {
		const url = getPermalinkUrl("my project", "web channel", "msg&id", "parent=id");
		expect(url).toBe("/my%20project?channel=web%20channel&thread=parent%3Did&msg=msg%26id");
	});

	it("returns empty string when projectName is missing", () => {
		expect(getPermalinkUrl(null as unknown as string, "web", "msg-123")).toBe("");
		expect(getPermalinkUrl("", "web", "msg-123")).toBe("");
	});

	it("returns empty string when channelName is missing", () => {
		expect(getPermalinkUrl("myproject", null as unknown as string, "msg-123")).toBe("");
		expect(getPermalinkUrl("myproject", "", "msg-123")).toBe("");
	});

	it("returns empty string when msgId is missing", () => {
		expect(getPermalinkUrl("myproject", "web", null as unknown as string)).toBe("");
		expect(getPermalinkUrl("myproject", "web", "")).toBe("");
	});
});

describe("getForkOwnerColor", () => {
	it("returns coworker avenue color for coworker fork session names", () => {
		// Fork name format: "{caller}-{slug}-{tid}" e.g. "park-discuss-ab12"
		expect(getForkOwnerColor("park-discuss-ab12")).toBe(AVENUE_COLORS.park);
		expect(getForkOwnerColor("madison-fix-bug-cd34")).toBe(AVENUE_COLORS.madison);
		expect(getForkOwnerColor("amsterdam-review-ef56")).toBe(AVENUE_COLORS.amsterdam);
	});

	it("returns lead gold for anonymous fork names (no caller prefix)", () => {
		// Fork name format: "fork-{tid}" e.g. "fork-abcdefgh"
		expect(getForkOwnerColor("fork-abcdefgh")).toBe(AVENUE_COLORS.lead);
		expect(getForkOwnerColor("fork-discuss-ab12")).toBe(AVENUE_COLORS.lead);
	});

	it("returns lead gold for channel lead fork names (non-avenue prefix)", () => {
		// Channel leads are named after channels, not avenues
		expect(getForkOwnerColor("web-discuss-ab12")).toBe(AVENUE_COLORS.lead);
		expect(getForkOwnerColor("design-review-cd34")).toBe(AVENUE_COLORS.lead);
	});

	it("returns lead gold for null/undefined/empty input", () => {
		expect(getForkOwnerColor(null)).toBe(AVENUE_COLORS.lead);
		expect(getForkOwnerColor(undefined)).toBe(AVENUE_COLORS.lead);
		expect(getForkOwnerColor("")).toBe(AVENUE_COLORS.lead);
	});

	it("returns lead gold for bare avenue names (no hyphen)", () => {
		// If the backend sends just "park" without compound fork name format,
		// getForkOwnerColor should still resolve the avenue color
		expect(getForkOwnerColor("park")).toBe(AVENUE_COLORS.park);
		expect(getForkOwnerColor("lead")).toBe(AVENUE_COLORS.lead);
	});
});

describe("senderChanged", () => {
	function msg(from: string, content?: string, tool_data?: unknown[]): Message {
		return { from, content: content ?? "", tool_data, id: "", timestamp: "" } as Message;
	}

	it("returns true for the first message", () => {
		expect(senderChanged([msg("park", "hello")], 0)).toBe(true);
	});

	it("returns false for consecutive messages from the same sender", () => {
		const msgs = [msg("park", "hello"), msg("park", "world")];
		expect(senderChanged(msgs, 1)).toBe(false);
	});

	it("returns true when sender differs from previous", () => {
		const msgs = [msg("park", "hello"), msg("madison", "world")];
		expect(senderChanged(msgs, 1)).toBe(true);
	});

	it("returns true when previous message is tool-only from the same sender", () => {
		// Agent's first message is tool-only (no text, has tool_data).
		// The next text message should show attribution even though sender matches.
		const msgs = [msg("park", "", [{ tool_name: "Bash", input: {} }]), msg("park", "Done!")];
		expect(senderChanged(msgs, 1)).toBe(true);
	});

	it("returns true when multiple tool-only predecessors from the same sender", () => {
		const msgs = [
			msg("park", "", [{ tool_name: "Read", input: {} }]),
			msg("park", "", [{ tool_name: "Bash", input: {} }]),
			msg("park", "All done"),
		];
		expect(senderChanged(msgs, 2)).toBe(true);
	});

	it("returns false when tool-only predecessor is from a different sender", () => {
		// Previous visible message is from park, tool-only from madison in between,
		// then another park message — should be continuation (false).
		const msgs = [
			msg("park", "starting"),
			msg("madison", "", [{ tool_name: "Bash", input: {} }]),
			msg("park", "continuing"),
		];
		expect(senderChanged(msgs, 2)).toBe(false);
	});

	it("returns true when all predecessors are tool-only (first visible message)", () => {
		const msgs = [msg("park", "", [{ tool_name: "Bash", input: {} }]), msg("park", "hello")];
		expect(senderChanged(msgs, 1)).toBe(true);
	});

	it("returns false for consecutive tool-only messages from the same sender (expanded tool run)", () => {
		// Inside expanded ToolRunSummary, tool-only messages are visible and should
		// use normal adjacent comparison to preserve sender grouping.
		const msgs = [
			msg("park", "", [{ tool_name: "Read", input: {} }]),
			msg("park", "", [{ tool_name: "Bash", input: {} }]),
		];
		expect(senderChanged(msgs, 1)).toBe(false);
	});
});
