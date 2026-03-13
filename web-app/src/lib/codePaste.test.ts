import { describe, expect, test, vi } from "vitest";
import { detectCode, handleCodePaste, wrapInFences } from "./codePaste.ts";

// ---------------------------------------------------------------------------
// detectCode
// ---------------------------------------------------------------------------

describe("detectCode", () => {
	test("detects multi-line JavaScript (function with body)", () => {
		const text = `function greet(name) {
	const msg = "Hello, " + name;
	return msg;
}`;
		const result = detectCode(text);
		expect(result.isCode).toBe(true);
	});

	test("detects multi-line Python (def with indentation)", () => {
		const text = `def fibonacci(n):
    if n <= 1:
        return n
    return fibonacci(n - 1) + fibonacci(n - 2)`;
		const result = detectCode(text);
		expect(result.isCode).toBe(true);
	});

	test("detects multi-line Rust (fn, struct, impl)", () => {
		const text = `pub struct Point {
    x: f64,
    y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}`;
		const result = detectCode(text);
		expect(result.isCode).toBe(true);
	});

	test("detects multi-line bash/shell", () => {
		const text = `#!/bin/bash
set -euo pipefail
echo "Building..."
cargo build --release | tee build.log`;
		const result = detectCode(text);
		expect(result.isCode).toBe(true);
	});

	test("detects multi-line JSON objects", () => {
		const text = `{
	"name": "midtown",
	"version": "1.0.0",
	"dependencies": {}
}`;
		const result = detectCode(text);
		expect(result.isCode).toBe(true);
	});

	test("does NOT detect regular English prose", () => {
		const text = `This is a paragraph of normal text that someone might paste
into a chat window. It has multiple lines but none of them look
like code at all. Just regular everyday conversation.`;
		const result = detectCode(text);
		expect(result.isCode).toBe(false);
	});

	test("does NOT detect a short sentence", () => {
		const result = detectCode("Hey, how are you doing today?");
		expect(result.isCode).toBe(false);
	});

	test("does NOT detect a URL", () => {
		const result = detectCode("https://github.com/user/repo/pull/123");
		expect(result.isCode).toBe(false);
	});

	test("does NOT detect text already in fences", () => {
		const text = "```js\nconst x = 1;\n```";
		const result = detectCode(text);
		expect(result.isCode).toBe(false);
	});

	test("detects single-line with strong signal (import statement)", () => {
		const result = detectCode("import React from 'react';");
		expect(result.isCode).toBe(true);
	});

	test("detects single-line with strong signal (const assignment)", () => {
		const result = detectCode("const handler = async (req, res) => {");
		expect(result.isCode).toBe(true);
	});

	test("does NOT detect single-line plain text", () => {
		const result = detectCode("Just a normal message");
		expect(result.isCode).toBe(false);
	});

	test("returns language when detectable", () => {
		const text = `function add(a, b) {
	return a + b;
}`;
		const result = detectCode(text);
		expect(result.isCode).toBe(true);
		// hljs should detect javascript or similar
		expect(result.language).toBeTruthy();
	});

	test("returns false for empty/null input", () => {
		expect(detectCode("").isCode).toBe(false);
		expect(detectCode(null).isCode).toBe(false);
		expect(detectCode("   ").isCode).toBe(false);
	});

	test("does NOT detect a file path", () => {
		const result = detectCode("/Users/someone/projects/midtown/src/main.rs");
		expect(result.isCode).toBe(false);
	});
});

// ---------------------------------------------------------------------------
// wrapInFences
// ---------------------------------------------------------------------------

describe("wrapInFences", () => {
	test("wraps with language tag", () => {
		const result = wrapInFences("const x = 1;", "javascript");
		expect(result).toBe("```javascript\nconst x = 1;\n```");
	});

	test("wraps without language tag", () => {
		const result = wrapInFences("const x = 1;", null);
		expect(result).toBe("```\nconst x = 1;\n```");
	});

	test("handles trailing newline in text (no double newline)", () => {
		const result = wrapInFences("const x = 1;\n", "js");
		expect(result).toBe("```js\nconst x = 1;\n```");
	});

	test("wraps multi-line text correctly", () => {
		const code = "function foo() {\n\treturn 42;\n}";
		const result = wrapInFences(code, "javascript");
		expect(result).toBe("```javascript\nfunction foo() {\n\treturn 42;\n}\n```");
	});
});

// ---------------------------------------------------------------------------
// handleCodePaste
// ---------------------------------------------------------------------------

describe("handleCodePaste", () => {
	function makeEvent(text) {
		return {
			clipboardData: {
				getData: vi.fn((type) => (type === "text/plain" ? text : "")),
			},
			preventDefault: vi.fn(),
		};
	}

	function makeTextarea(value, selectionStart, selectionEnd) {
		return {
			selectionStart: selectionStart ?? value.length,
			selectionEnd: selectionEnd ?? value.length,
		};
	}

	test("returns false for non-code text", () => {
		const e = makeEvent("Hello, this is a normal message.");
		const textarea = makeTextarea("");
		const getText = () => "";
		const setText = vi.fn();

		const result = handleCodePaste(e, textarea, getText, setText);
		expect(result).toBe(false);
		expect(e.preventDefault).not.toHaveBeenCalled();
		expect(setText).not.toHaveBeenCalled();
	});

	test("returns cursor position and calls preventDefault + setText for code", () => {
		const code = `function greet(name) {
	const msg = "Hello, " + name;
	return msg;
}`;
		const e = makeEvent(code);
		const textarea = makeTextarea("", 0, 0);
		const getText = () => "";
		const setText = vi.fn();

		const result = handleCodePaste(e, textarea, getText, setText);
		expect(result).not.toBe(false);
		expect(typeof result).toBe("number");
		expect(e.preventDefault).toHaveBeenCalled();
		expect(setText).toHaveBeenCalledTimes(1);
		const newText = setText.mock.calls[0][0];
		expect(newText).toContain("```");
		expect(newText).toContain("function greet(name)");
	});

	test("returns false for already-fenced text", () => {
		const text = "```js\nconst x = 1;\n```";
		const e = makeEvent(text);
		const textarea = makeTextarea("");
		const getText = () => "";
		const setText = vi.fn();

		const result = handleCodePaste(e, textarea, getText, setText);
		expect(result).toBe(false);
		expect(e.preventDefault).not.toHaveBeenCalled();
	});

	test("returns false when no clipboard text", () => {
		const e = makeEvent("");
		const textarea = makeTextarea("");
		const getText = () => "";
		const setText = vi.fn();

		const result = handleCodePaste(e, textarea, getText, setText);
		expect(result).toBe(false);
	});

	test("returns false when clipboardData is null", () => {
		const e = { clipboardData: null, preventDefault: vi.fn() };
		const textarea = makeTextarea("");
		const getText = () => "";
		const setText = vi.fn();

		const result = handleCodePaste(e, textarea, getText, setText);
		expect(result).toBe(false);
	});

	test("inserts at cursor position correctly", () => {
		const code = `const x = () => {
	return 42;
};`;
		const e = makeEvent(code);
		const existing = "before  after";
		const textarea = makeTextarea(existing, 7, 7); // cursor between the two spaces
		const getText = () => existing;
		const setText = vi.fn();

		const result = handleCodePaste(e, textarea, getText, setText);
		expect(result).not.toBe(false);
		const newText = setText.mock.calls[0][0];
		expect(newText.startsWith("before ")).toBe(true);
		expect(newText.endsWith(" after")).toBe(true);
		expect(newText).toContain("```");
	});

	test("returns correct cursor position after fenced block", () => {
		const code = "const x = 1;";
		const e = makeEvent(code);
		const textarea = makeTextarea("", 0, 0);
		const getText = () => "";
		const setText = vi.fn();

		const cursorPos = handleCodePaste(e, textarea, getText, setText);
		const newText = setText.mock.calls[0][0];
		// Cursor should be at end of the inserted fenced block
		expect(cursorPos).toBe(newText.length);
	});

	test("returns correct cursor position when inserting mid-text", () => {
		const code = "const x = 1;";
		const e = makeEvent(code);
		const existing = "before  after";
		const textarea = makeTextarea(existing, 7, 7);
		const getText = () => existing;
		const setText = vi.fn();

		const cursorPos = handleCodePaste(e, textarea, getText, setText);
		const fenced = wrapInFences(code, null);
		// Cursor should be at start offset + fenced block length
		expect(cursorPos).toBe(7 + fenced.length);
	});
});
