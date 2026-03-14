// Shared paste/upload logic used by Channel.svelte and ThreadPanel.svelte.

import { sendMessage, uploadFile } from "./api.ts";

/**
 * Handle a paste event — extract a file (image or other) from the clipboard.
 * Returns the File if one was found, or null otherwise.
 * Calls e.preventDefault() when a file is consumed.
 */
export function extractPastedFile(e: ClipboardEvent): File | null {
	const items = e.clipboardData?.items;
	if (!items) return null;

	for (const item of items) {
		if (item.type.startsWith("image/") || item.kind === "file") {
			const file = item.getAsFile();
			if (file) {
				e.preventDefault();
				return file;
			}
		}
	}
	return null;
}

/**
 * Create a blob preview URL for a file, revoking any previous URL to prevent leaks.
 * Returns the new URL, or null if file is null/undefined.
 */
export function updatePreviewUrl(previousUrl: string | null, file: File | null): string | null {
	if (previousUrl) URL.revokeObjectURL(previousUrl);
	return file ? URL.createObjectURL(file) : null;
}

/**
 * Upload a pending file and send it as a message.
 *
 * @param {File} file - The file to upload
 * @param {string} text - Optional message text to accompany the file
 * @param {string} channel - Channel name
 * @param {string|null} threadParentId - Thread parent message ID (null for top-level)
 * @returns {Promise<{ok: boolean, error?: string}>}
 */
export async function uploadAndSend(
	file: File,
	text: string,
	channel: string,
	threadParentId: string | null = null,
): Promise<{ ok: boolean; error?: string }> {
	const result = await uploadFile(file);

	if (!result.ok) {
		return { ok: false, error: result.error };
	}

	const message = text.trim()
		? `${text.trim()}\n\n[Attached: ${result.path}]`
		: `[Attached file: ${result.filename}]\nPlease read: ${result.path}`;

	sendMessage(message, channel, threadParentId);
	return { ok: true };
}
