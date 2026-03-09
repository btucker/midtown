<script>
import { fade, slide } from "svelte/transition";
import MessageRow from "./MessageRow.svelte";
import { createAutoCollapse } from "./useAutoCollapse.js";

const TOOL_RUN_DELAY_MS = 60_000;

let {
	messages,
	toolCount,
	lastTimestamp,
	allMessages = [],
	startIndex = 0,
	channelName = undefined,
	currentTasks = {},
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

function toggle() {
	userOverride = true;
	ac.clearTimer();
	displayState = displayState === "expanded" ? "collapsed" : "expanded";
}
</script>

{#if displayState === "collapsed"}
	<button
		class="tool-run-summary"
		onclick={toggle}
		in:fade={{ duration: mounted ? 150 : 0, delay: mounted ? 150 : 0 }}
	>
		<span class="tool-run-icon">▸</span>
		<span class="tool-run-text">{toolCount} tools used</span>
	</button>
{:else}
	<div
		class="tool-run-expanded"
		out:slide={{ duration: 200 }}
	>
		<button class="tool-run-summary tool-run-expanded-header" onclick={toggle}>
			<span class="tool-run-icon">▾</span>
			<span class="tool-run-text">{toolCount} tools used</span>
		</button>
		{#each messages as msg, i}
			<MessageRow
				{msg}
				msgs={allMessages}
				index={startIndex + i}
				senderClass="mt-1"
				{channelName}
				currentTask={currentTasks[msg.from?.toLowerCase()]}
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
