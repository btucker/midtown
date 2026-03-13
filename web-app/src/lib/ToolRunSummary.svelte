<script>
import { fade, slide } from "svelte/transition";
import MessageRow from "./MessageRow.svelte";
import { filterChannelPosts } from "./toolRunGrouping.js";
import { createAutoCollapse } from "./useAutoCollapse.js";

const TOOL_RUN_DELAY_MS = 10_000;

let {
	messages,
	toolCount,
	lastTimestamp,
	allMessages = [],
	startIndex = 0,
	channelName = undefined,
	currentTasks = {},
	showToolData = true,
} = $props();

let displayState = $state("collapsed");
let userOverride = $state(false);

const ac = $derived.by(() => createAutoCollapse(lastTimestamp, TOOL_RUN_DELAY_MS));

$effect.pre(() => {
	if (!userOverride) displayState = ac.initial === "collapsed" ? "collapsed" : "expanded";
});

// Skip intro transitions on initial mount (avoid animating already-collapsed items)
let mounted = $state(false);
$effect(() => {
	mounted = true;
});

$effect(() => {
	if (userOverride) return;
	const currentAc = ac;
	currentAc.startTimer(() => {
		displayState = "collapsed";
	});
	return () => currentAc.clearTimer();
});

// Filter out 'midtown channel post' blocks from expanded view — they're redundant
// since the posted message already appears in the channel.
// Preserves original indexes so MessageRow gets the correct position in allMessages.
let visibleEntries = $derived.by(() =>
	messages
		.map((msg, i) => {
			const filtered = filterChannelPosts(msg.tool_data);
			if (filtered.length === msg.tool_data?.length) return { msg, origIndex: i };
			if (filtered.length === 0) return null;
			return { msg: { ...msg, tool_data: filtered }, origIndex: i };
		})
		.filter(Boolean),
);

// Use visible tool count (after filtering channel posts) instead of raw toolCount
let visibleToolCount = $derived(visibleEntries.reduce((sum, entry) => sum + (entry.msg.tool_data?.length || 0), 0));

function toggle() {
	userOverride = true;
	ac.clearTimer();
	displayState = displayState === "expanded" ? "collapsed" : "expanded";
}
</script>

{#if visibleToolCount === 0}
	<!-- All tool blocks were filtered (e.g. channel posts) — render nothing -->
{:else if displayState === "collapsed"}
	<button
		class="tool-run-summary"
		onclick={toggle}
		in:fade={{ duration: mounted ? 150 : 0, delay: mounted ? 150 : 0 }}
		out:fade={{ duration: mounted ? 100 : 0 }}
	>
		<span class="tool-run-icon">▸</span>
		<span class="tool-run-text">{visibleToolCount} {visibleToolCount === 1 ? 'tool' : 'tools'} used</span>
	</button>
{:else}
	<div
		class="tool-run-expanded"
		in:slide={{ duration: mounted ? 200 : 0 }}
		out:slide={{ duration: 200 }}
	>
		<button class="tool-run-summary tool-run-expanded-header" onclick={toggle}>
			<span class="tool-run-icon">▾</span>
			<span class="tool-run-text">{visibleToolCount} {visibleToolCount === 1 ? 'tool' : 'tools'} used</span>
		</button>
		{#each visibleEntries as entry}
			<MessageRow
				msg={entry.msg}
				msgs={allMessages}
				index={startIndex + entry.origIndex}
				senderClass="mt-1"
				{channelName}
				currentTask={currentTasks[entry.msg.from?.toLowerCase()]}
				{showToolData}
			/>
		{/each}
	</div>
{/if}

<style>
	.tool-run-summary {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 4px 10px;
		margin: 4px 0 4px calc(2.4rem + 0.5rem);
		background: transparent;
		border: 1px solid hsl(var(--border) / 0.5);
		border-radius: 12px;
		cursor: pointer;
		font-family: var(--font-mono);
		font-size: 0.72rem;
		color: hsl(var(--muted-foreground));
		width: fit-content;
		transition: background 0.15s;
	}

	.tool-run-summary:hover {
		background: hsl(var(--accent) / 0.5);
	}

	.tool-run-icon {
		font-size: 0.65rem;
	}

	.tool-run-expanded-header {
		margin-bottom: 0;
	}

	.tool-run-expanded {
		overflow: hidden;
	}
</style>
