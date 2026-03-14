import { describe, expect, it, vi } from "vitest";
import { getCommandNames, parseCommand } from "./commands.ts";

// Mock api and store modules
vi.mock("./api.ts", () => ({
	archiveChannel: vi.fn(),
	unarchiveChannel: vi.fn(),
	fetchChannels: vi.fn(),
}));

vi.mock("./store.ts", () => ({
	activeChannel: {
		subscribe: vi.fn((fn) => {
			fn("test-channel");
			return () => {};
		}),
	},
}));

vi.mock("svelte/store", () => ({
	get: () => "test-channel",
}));

describe("parseCommand", () => {
	it("returns handled: false for non-command input", () => {
		expect(parseCommand("hello world")).toEqual({ handled: false });
		expect(parseCommand("")).toEqual({ handled: false });
		expect(parseCommand("no slash here")).toEqual({ handled: false });
	});

	it("returns handled: false for unknown commands", () => {
		expect(parseCommand("/unknown")).toEqual({ handled: false });
		expect(parseCommand("/foo bar")).toEqual({ handled: false });
	});

	it("recognizes /archive command", () => {
		const result = parseCommand("/archive");
		expect(result.handled).toBe(true);
		expect(result.command).toBe("archive");
		expect(result.needsConfirmation).toBe(true);
		expect(typeof result.execute).toBe("function");
	});

	it("recognizes /unarchive command", () => {
		const result = parseCommand("/unarchive");
		expect(result.handled).toBe(true);
		expect(result.command).toBe("unarchive");
		expect(result.needsConfirmation).toBeFalsy();
		expect(typeof result.execute).toBe("function");
	});

	it("handles /archive with leading/trailing whitespace", () => {
		const result = parseCommand("  /archive  ");
		expect(result.handled).toBe(true);
		expect(result.command).toBe("archive");
	});

	it("is case-insensitive for command names", () => {
		const result = parseCommand("/ARCHIVE");
		expect(result.handled).toBe(true);
		expect(result.command).toBe("archive");
	});

	it("/archive needs confirmation with channel name in message", () => {
		const result = parseCommand("/archive");
		expect(result.confirmMessage).toContain("test-channel");
	});
});

describe("getCommandNames", () => {
	it("returns all registered commands", () => {
		const commands = getCommandNames();
		expect(commands.length).toBeGreaterThanOrEqual(2);
		expect(commands.find((c) => c.name === "archive")).toBeTruthy();
		expect(commands.find((c) => c.name === "unarchive")).toBeTruthy();
	});

	it("each command has name and description", () => {
		const commands = getCommandNames();
		for (const cmd of commands) {
			expect(cmd.name).toBeTruthy();
			expect(cmd.description).toBeTruthy();
		}
	});
});
