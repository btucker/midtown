/**
 * Shared syntax highlighting configuration.
 *
 * This module provides a pre-configured highlight.js instance with
 * all relevant languages registered. Use this instead of importing
 * hljs directly to ensure languages are available.
 */
import hljs from "highlight.js/lib/core";
import bash from "highlight.js/lib/languages/bash";
import css from "highlight.js/lib/languages/css";
import diff from "highlight.js/lib/languages/diff";
import toml from "highlight.js/lib/languages/ini"; // hljs uses 'ini' for TOML
import javascript from "highlight.js/lib/languages/javascript";
import json from "highlight.js/lib/languages/json";
import python from "highlight.js/lib/languages/python";
// Register only languages relevant to this project (tree-shakeable imports)
import rust from "highlight.js/lib/languages/rust";
import typescript from "highlight.js/lib/languages/typescript";
import xml from "highlight.js/lib/languages/xml";
import yaml from "highlight.js/lib/languages/yaml";

hljs.registerLanguage("rust", rust);
hljs.registerLanguage("javascript", javascript);
hljs.registerLanguage("js", javascript);
hljs.registerLanguage("typescript", typescript);
hljs.registerLanguage("ts", typescript);
hljs.registerLanguage("python", python);
hljs.registerLanguage("py", python);
hljs.registerLanguage("bash", bash);
hljs.registerLanguage("sh", bash);
hljs.registerLanguage("shell", bash);
hljs.registerLanguage("json", json);
hljs.registerLanguage("toml", toml);
hljs.registerLanguage("yaml", yaml);
hljs.registerLanguage("yml", yaml);
hljs.registerLanguage("css", css);
hljs.registerLanguage("xml", xml);
hljs.registerLanguage("html", xml);
hljs.registerLanguage("diff", diff);

/**
 * Escape HTML special characters for safe rendering.
 * @param {string} str - Raw text to escape
 * @returns {string} HTML-escaped text
 */
export function escapeHtml(str: string): string {
	return str.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

/**
 * Highlight a single line of code.
 * @param {string} text - The code to highlight
 * @param {string|null} lang - Language name (e.g., 'rust', 'bash') or null for auto-detection
 * @returns {string} HTML with syntax highlighting spans
 */
export function highlightLine(text: string, lang: string | null): string {
	if (!lang || !hljs.getLanguage(lang)) {
		// Try auto-detection for unknown languages
		try {
			return hljs.highlightAuto(text).value;
		} catch {
			return escapeHtml(text);
		}
	}
	try {
		return hljs.highlight(text, { language: lang }).value;
	} catch {
		return escapeHtml(text);
	}
}

/**
 * Highlight a block of code (multiple lines).
 * @param {string} code - The code block to highlight
 * @param {string|null} lang - Language name or null for auto-detection
 * @returns {string} HTML with syntax highlighting spans
 */
export function highlightBlock(code: string, lang: string | null): string {
	if (!lang || !hljs.getLanguage(lang)) {
		try {
			return hljs.highlightAuto(code).value;
		} catch {
			return escapeHtml(code);
		}
	}
	try {
		return hljs.highlight(code, { language: lang }).value;
	} catch {
		return escapeHtml(code);
	}
}

/**
 * Map file extensions to highlight.js language names.
 * Kept in sync with the languages registered above.
 */
export const EXT_TO_LANG: Record<string, string> = {
	rs: "rust",
	js: "javascript",
	jsx: "javascript",
	ts: "typescript",
	tsx: "typescript",
	py: "python",
	sh: "bash",
	bash: "bash",
	zsh: "bash",
	json: "json",
	toml: "toml",
	yaml: "yaml",
	yml: "yaml",
	css: "css",
	svelte: "xml",
	html: "xml",
	xml: "xml",
	md: "xml",
};

/**
 * Get the highlight.js language name for a file path based on its extension.
 * @param {string} path - File path
 * @returns {string|null} Language name or null if unknown
 */
export function getLanguage(path: string | null | undefined): string | null {
	if (!path) return null;
	const ext = path.split(".").pop()?.toLowerCase();
	return ext ? EXT_TO_LANG[ext] || null : null;
}

// Export the configured hljs instance for advanced use cases
export { hljs };
