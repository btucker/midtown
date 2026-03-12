/**
 * Check if a message contains only tool calls (no meaningful text content).
 */
export function isToolOnly(msg) {
	return (!msg.content || !msg.content.trim()) && msg.tool_data?.length > 0;
}

/**
 * Check if a tool block is a 'midtown channel post' Bash command.
 * These are redundant in the UI since the posted message already appears in the channel.
 */
export function isChannelPostBlock(block) {
	return (
		block.tool_name === "Bash" &&
		typeof block.input?.command === "string" &&
		block.input.command.includes("midtown channel post")
	);
}

/**
 * Filter out channel-post tool blocks from a message's tool_data.
 * Returns the filtered array (may be empty if all blocks were channel posts).
 */
export function filterChannelPosts(toolData) {
	if (!toolData) return [];
	return toolData.filter((block) => !isChannelPostBlock(block));
}

/**
 * Group consecutive tool-only messages into collapsible "tool runs".
 *
 * Returns an array of segments:
 *   { type: 'message', message }           — a normal message
 *   { type: 'tool-run', messages, toolCount, lastTimestamp } — 1+ consecutive tool-only messages
 */
export function groupToolRuns(messages) {
	const segments = [];
	let currentRun = [];

	function flushRun() {
		if (currentRun.length >= 1) {
			const toolCount = currentRun.reduce((sum, m) => sum + (m.tool_data?.length || 0), 0);
			segments.push({
				type: "tool-run",
				messages: currentRun,
				toolCount,
				lastTimestamp: currentRun[currentRun.length - 1].timestamp,
			});
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

/**
 * Group consecutive tool-only timeline entries into collapsible runs.
 * Works on the merged timeline array from ThreadPanel (entries have {type, data, timestamp, msgIndex}).
 * Edit entries and text message entries break runs.
 * Single tool-only entries ARE grouped (collapsed like multi-tool runs).
 */
export function groupTimelineToolRuns(timeline) {
	const segments = [];
	let currentRun = [];

	function flushRun() {
		if (currentRun.length >= 1) {
			const toolCount = currentRun.reduce((sum, e) => sum + (e.data.tool_data?.length || 0), 0);
			segments.push({
				type: "tool-run",
				entries: currentRun,
				toolCount,
				lastTimestamp: currentRun[currentRun.length - 1].data.timestamp,
			});
		}
		currentRun = [];
	}

	for (const entry of timeline) {
		if (entry.type === "message" && isToolOnly(entry.data)) {
			currentRun.push(entry);
		} else {
			flushRun();
			segments.push(entry);
		}
	}
	flushRun();
	return segments;
}
