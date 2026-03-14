// Smart code paste detection and markdown fence wrapping.

import { hljs } from "./highlighting.ts";

/** Keywords that strongly suggest the text is code. */
const CODE_KEYWORDS =
	/\b(function|import|export|default|const|let|var|return|async|await|class|extends|interface|type|enum|def|self|lambda|yield|struct|impl|pub|fn|mod|crate|use|trait|match|#include|#define|#ifdef|printf|int\s+main|void)\b/;

/** Shebang line at the start of the text. */
const SHEBANG = /^#!\//;

/** Patterns that suggest code syntax (operators, brackets, arrows, etc.) */
const CODE_SYNTAX = /[{}();]|=>|->|\|\||&&|===?|!==?|<<|>>/;

/** Single-line patterns strong enough to trigger detection on their own. */
const STRONG_SINGLE_LINE =
	/^\s*(import\s+.+from\s+|const\s+\w+\s*=|let\s+\w+\s*=|var\s+\w+\s*=|export\s+(default\s+)?|function\s+\w+|def\s+\w+|fn\s+\w+|pub\s+fn|#include\s+[<"]|from\s+\w+\s+import\s+)/;

/**
 * Detect whether pasted text looks like code.
 *
 * @param {string} text - The pasted text to analyse
 * @returns {{ isCode: boolean, language: string | null }}
 */
export function detectCode(text: string): { isCode: boolean; language: string | null } {
	if (!text || !text.trim()) {
		return { isCode: false, language: null };
	}

	// Already fenced — don't double-wrap.
	if (/^```/m.test(text)) {
		return { isCode: false, language: null };
	}

	const lines = text.split("\n");
	const nonEmptyLines = lines.filter((l) => l.trim().length > 0);

	// Pure URL or path — not code.
	if (nonEmptyLines.length === 1) {
		const trimmed = nonEmptyLines[0].trim();
		if (/^https?:\/\/\S+$/.test(trimmed) || /^[/~][\w\-/.]+$/.test(trimmed)) {
			return { isCode: false, language: null };
		}
	}

	// --- Heuristic signals ---
	let score = 0;

	// 1. Shebang
	if (SHEBANG.test(text)) score += 5;

	// 2. Keyword matches
	if (CODE_KEYWORDS.test(text)) score += 3;

	// 3. Syntax characters
	if (CODE_SYNTAX.test(text)) score += 2;

	// 4. Consistent indentation (>= 3 lines with leading whitespace)
	const indentedLines = nonEmptyLines.filter((l) => /^[\t ]{2,}/.test(l));
	if (nonEmptyLines.length >= 3 && indentedLines.length >= 2) score += 3;

	// 5. Semicolons at end of lines
	const semiLines = nonEmptyLines.filter((l) => /;\s*$/.test(l));
	if (semiLines.length >= 2) score += 2;

	// 6. highlight.js relevance
	let hljsLanguage: string | null | undefined = null;
	try {
		const result = hljs.highlightAuto(text);
		if (result.relevance > 5) {
			score += 3;
			hljsLanguage = result.language || null;
		} else if (result.relevance > 2) {
			score += 1;
			hljsLanguage = result.language || null;
		}
	} catch {
		// hljs failed — ignore
	}

	// --- Threshold depends on line count ---
	const isMultiLine = nonEmptyLines.length >= 3;

	if (isMultiLine) {
		// Multi-line: moderate threshold
		if (score >= 4) {
			return { isCode: true, language: hljsLanguage };
		}
	} else {
		// Short text: require strong signal or single-line pattern match
		if (STRONG_SINGLE_LINE.test(text) || score >= 6) {
			return { isCode: true, language: hljsLanguage };
		}
	}

	return { isCode: false, language: null };
}

/**
 * Wrap text in markdown code fences.
 *
 * @param {string} text - The code text
 * @param {string|null} language - Language tag (e.g. "javascript"), or null
 * @returns {string} Fenced code block
 */
export function wrapInFences(text: string, language: string | null): string {
	const lang = language || "";
	const body = text.trimEnd();
	return `\`\`\`${lang}\n${body}\n\`\`\``;
}

/**
 * Handle a paste event — detect code and wrap in markdown fences.
 *
 * @param {ClipboardEvent} e - The paste event
 * @param {HTMLTextAreaElement} textareaElement - The textarea element
 * @param {() => string} getCurrentText - Returns the current textarea value
 * @param {(text: string) => void} setText - Sets the textarea value
 * @returns {false | number} `false` if the paste was not handled, or the
 *   cursor position that should be set after the DOM updates.
 */
export function handleCodePaste(
	e: ClipboardEvent,
	textareaElement: HTMLTextAreaElement,
	getCurrentText: () => string,
	setText: (text: string) => void,
): false | number {
	const text = e.clipboardData?.getData("text/plain");
	if (!text) return false;

	const { isCode, language } = detectCode(text);
	if (!isCode) return false;

	e.preventDefault();

	const fenced = wrapInFences(text, language);
	const current = getCurrentText();
	const start = textareaElement.selectionStart ?? current.length;
	const end = textareaElement.selectionEnd ?? current.length;

	const newText = current.slice(0, start) + fenced + current.slice(end);
	setText(newText);

	return start + fenced.length;
}
