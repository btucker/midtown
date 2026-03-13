import { get } from "svelte/store";
import { archiveChannel, fetchChannels, unarchiveChannel } from "./api.ts";
import { activeChannel, showArchivedChannels } from "./store.ts";

/**
 * Registry of slash commands available in the web UI.
 * Each command has a name, description, and execute function.
 * The execute function receives { args, channel } and returns { ok, message?, error? }.
 */
const commands = [
	{
		name: "archive",
		description: "Archive the current channel",
		async execute({ channel }) {
			if (!channel) {
				return { ok: false, error: "No active channel" };
			}
			const result = await archiveChannel(channel);
			if (result.ok) {
				return { ok: true, message: `Channel #${channel} archived` };
			}
			return { ok: false, error: result.error || "Failed to archive channel" };
		},
	},
	{
		name: "unarchive",
		description: "Unarchive a channel",
		async execute({ args, channel }) {
			const target = args.trim() || channel;
			if (!target) {
				return { ok: false, error: "Usage: /unarchive <channel-name>" };
			}
			const result = await unarchiveChannel(target);
			if (result.ok) {
				await fetchChannels(get(showArchivedChannels));
				return { ok: true, message: `Channel #${target} unarchived` };
			}
			return { ok: false, error: result.error || "Failed to unarchive channel" };
		},
	},
];

/**
 * Parse input text and check if it's a slash command.
 * Returns { handled, execute? } where execute is an async function if handled.
 */
export function parseCommand(input) {
	const trimmed = input.trim();
	if (!trimmed.startsWith("/")) {
		return { handled: false };
	}

	const spaceIndex = trimmed.indexOf(" ");
	const name = spaceIndex === -1 ? trimmed.slice(1) : trimmed.slice(1, spaceIndex);
	const args = spaceIndex === -1 ? "" : trimmed.slice(spaceIndex + 1);

	const command = commands.find((cmd) => cmd.name === name.toLowerCase());
	if (!command) {
		return { handled: false };
	}

	return {
		handled: true,
		command: command.name,
		needsConfirmation: command.name === "archive",
		confirmMessage: `Archive channel #${get(activeChannel)}? This will remove it from the sidebar.`,
		async execute() {
			const channel = get(activeChannel);
			return command.execute({ args, channel });
		},
	};
}

/**
 * Get all registered command names (for autocomplete).
 */
export function getCommandNames() {
	return commands.map((cmd) => ({ name: cmd.name, description: cmd.description }));
}
