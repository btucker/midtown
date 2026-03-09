/**
 * Check if a message contains only tool calls (no meaningful text content).
 */
function isToolOnly(msg) {
	return (!msg.content || !msg.content.trim()) && msg.tool_data?.length > 0;
}

/**
 * Group consecutive tool-only messages into collapsible "tool runs".
 *
 * Returns an array of segments:
 *   { type: 'message', message }           — a normal message
 *   { type: 'tool-run', messages, toolCount, lastTimestamp } — 2+ consecutive tool-only messages
 *
 * Single tool-only messages are returned as regular 'message' segments.
 */
export function groupToolRuns(messages) {
	const segments = [];
	let currentRun = [];

	function flushRun() {
		if (currentRun.length >= 2) {
			const toolCount = currentRun.reduce((sum, m) => sum + (m.tool_data?.length || 0), 0);
			segments.push({
				type: "tool-run",
				messages: currentRun,
				toolCount,
				lastTimestamp: currentRun[currentRun.length - 1].timestamp,
			});
		} else if (currentRun.length === 1) {
			segments.push({ type: "message", message: currentRun[0] });
		}
		currentRun = [];
	}

	for (const msg of messages) {
		if (isToolOnly(msg)) {
			currentRun.push(msg);
		} else {
			flushRun();
			segments.push({ type: "message", message: msg });
		}
	}
	flushRun();

	return segments;
}
