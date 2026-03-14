<script lang="ts">
import GitFork from "@lucide/svelte/icons/git-fork";
import { getApiBase, selectDm } from "./api.ts";
import { getDisplayableDmChannels } from "./channelUtils.ts";
import { renderContent } from "./markdown.ts";
import {
	formatTime,
	formatTimeCompact,
	getPermalinkUrl,
	getSenderColor,
	isDimSender,
	parseInsightSegments,
	senderChanged,
	timeChanged,
} from "./messageUtils.ts";
import { activeProject, channels, coworkers } from "./store.ts";
import ToolDataBlocks from "./ToolDataBlocks.svelte";
import { isToolOnly } from "./toolRunGrouping.ts";

const AVATAR_SIZE = "2.4rem";
const AVATAR_GAP = "0.5rem";

let {
	msg,
	msgs,
	index,
	senderOverrides = undefined,
	dimSenders = undefined,
	senderSpacing = "1.5em",
	senderClass = "",
	currentTask = undefined,
	channelName = undefined,
	threadParentId = undefined,
	isDedicatedSession = false,
	forkParentLead = undefined,
	showToolData = true,
	class: extraClass = "",
	children = undefined,
} = $props();

const TASK_DIVIDER_RE = /^─── Task !.+───$/;

function isTaskDivider(msg) {
	return msg.from === "midtown" && TASK_DIVIDER_RE.test((msg.content || "").trim());
}

// renderContent() wraps output in block-level <p> tags via marked.parse().
// Strip the outer <p>...</p> so the label can sit inline within the divider flex row.
function renderInline(text) {
	return renderContent(text, getApiBase())
		.replace(/^<p>/, "")
		.replace(/<\/p>\s*$/, "");
}

function avatarLetter(name) {
	return (name || "?")[0].toUpperCase();
}

// When in a dedicated fork session with a known parent lead, display the
// parent lead's name/color instead of the fork session's "fork-XXXX" name.
let isForkWithParent = $derived(isDedicatedSession && !!forkParentLead);
let displayName = $derived(isForkWithParent ? forkParentLead : msg.from);
let displayColor = $derived(getSenderColor(displayName, senderOverrides, channelName));
// For click navigation, always use msg.from (the actual session name) so
// fork messages navigate to dm-<forkName>, not dm-<parentLeadName>.
let clickName = $derived(msg.from);

const agentNames = $derived(
	new Set([
		// Agents with existing displayable DM channels (covers coworkers, forks,
		// and historical DMs that don't shadow a real channel lead home).
		...getDisplayableDmChannels($channels).map((ch) => ch.name.slice(3)),
		// Active coworkers (selectDm creates channel on demand)
		...$coworkers.map((cw) => cw.name),
	]),
);

function handleSenderClick() {
	if (agentNames.has(clickName)) selectDm(clickName);
}

let permalinkUrl = $derived(
	channelName && msg?.id ? getPermalinkUrl($activeProject, channelName, msg.id, threadParentId) : "",
);

let copiedTooltip = $state(false);
let tooltipTimeout = null;

// Clean up tooltip timeout when component is destroyed
$effect(() => {
	return () => {
		if (tooltipTimeout) clearTimeout(tooltipTimeout);
	};
});

function handleTimestampClick(e) {
	if (!permalinkUrl) return;
	e.preventDefault();
	e.stopPropagation();
	const fullUrl = window.location.origin + permalinkUrl;
	navigator.clipboard.writeText(fullUrl).then(() => {
		copiedTooltip = true;
		if (tooltipTimeout) clearTimeout(tooltipTimeout);
		tooltipTimeout = setTimeout(() => {
			copiedTooltip = false;
		}, 1500);
	});
}

function isAction(msg) {
	return msg.msg_type === "action" || msg.content?.startsWith("/me ");
}

function getActionContent(msg) {
	return msg.content.replace(/^\/me\s*/, "");
}

// When tool data is hidden and the message has no text content, skip rendering
// entirely to avoid blank rows (avatar + timestamp with no visible payload).
let hidden = $derived(!showToolData && isToolOnly(msg));
</script>

{#if hidden}
  <!-- Tool-only message with tool data hidden: render nothing -->
{:else if isTaskDivider(msg)}
  <!-- Task divider: centered HR with task link -->
  <div class="flex items-center gap-2 py-3 text-muted-foreground/50 text-[0.72rem] select-none">
    <div class="flex-1 h-px bg-border/60"></div>
    <span>{@html renderInline(msg.content.replace(/^───\s*/, '').replace(/\s*───$/, ''))}</span>
    <div class="flex-1 h-px bg-border/60"></div>
  </div>
{:else if senderChanged(msgs, index)}
  <div class="flex items-start gap-[0.5rem] pt-[3px] {senderClass} {extraClass}" data-msg-id={msg.id} style={index > 0 ? `margin-top: ${senderSpacing}` : ''}>
    <!-- Avatar -->
    <div
      class="relative flex-shrink-0 {agentNames.has(clickName) ? 'cursor-pointer' : ''}"
      style="width: {AVATAR_SIZE}; height: {AVATAR_SIZE}"
      onclick={() => handleSenderClick()}
      role={agentNames.has(clickName) ? 'button' : undefined}
      title={agentNames.has(clickName) ? `Open DM with ${displayName}` : undefined}
    >
      <div
        class="rounded-md flex items-center justify-center text-white font-bold text-[1rem] select-none mt-[0.15rem]"
        style="width: {AVATAR_SIZE}; height: {AVATAR_SIZE}; background-color: {displayColor}"
      >{avatarLetter(displayName)}</div>
      {#if isDedicatedSession}
        <div
          class="absolute -bottom-1 -right-1 flex items-center justify-center w-4 h-4 rounded-full bg-background border border-border"
          title={forkParentLead ? `Fork of ${forkParentLead}` : 'Fork session'}
        >
          <GitFork size={10} class="text-muted-foreground" />
        </div>
      {/if}
    </div>
    <!-- Header + content -->
    <div class="flex-1 min-w-0">
      <div class="whitespace-nowrap overflow-hidden text-ellipsis flex items-baseline gap-3">
        {#if agentNames.has(clickName)}
          <button
            class="font-mono font-semibold text-[1rem] text-foreground bg-transparent border-none p-0 m-0 cursor-pointer hover:underline"
            data-testid="message-sender"
            onclick={() => selectDm(clickName)}
            title="Open DM with {displayName}"
          >{displayName}</button>
        {:else}
          <span
            class="font-mono font-semibold text-[1rem] text-foreground"
            data-testid="message-sender"
          >{displayName}</span>
        {/if}
        {#if permalinkUrl}
          <a
            href={permalinkUrl}
            class="timestamp-link text-muted-foreground/70 text-[0.7rem] select-none no-underline hover:text-muted-foreground relative"
            data-testid="message-time"
            onclick={handleTimestampClick}
          >
            {formatTime(msg.timestamp)}
            {#if copiedTooltip}
              <span class="copied-tooltip">Link copied!</span>
            {/if}
          </a>
        {:else}
          <span class="text-muted-foreground/70 text-[0.7rem] select-none" data-testid="message-time">
            {formatTime(msg.timestamp)}
          </span>
        {/if}
        {#if currentTask}
          <span class="text-muted-foreground text-[0.7rem]"> — {currentTask}</span>
        {/if}
      </div>
      {#if children}
        {@render children()}
      {:else}
        {#if isAction(msg)}
          <div class="flex gap-0 break-words">
            <span class="flex-shrink-0 mr-[0.3em]" style="color: {displayColor}">*</span>
            <span class="action-text flex-1 min-w-0" style="color: {displayColor}">{@html renderContent(getActionContent(msg), getApiBase())}</span>
          </div>
        {:else}
          {#each parseInsightSegments(msg.content || '') as segment}
            {#if segment.type === 'insight'}
              <div class="border-l-2 pl-3 max-w-[85%] my-0.5" style="border-color: {displayColor}80">
                <div class="message-text text-foreground">{@html renderContent(segment.content, getApiBase())}</div>
              </div>
            {:else if segment.content.trim()}
              <div class="break-words message-text {isDimSender(msg.from, dimSenders) ? 'text-muted-foreground' : 'text-foreground'}">{@html renderContent(segment.content, getApiBase())}</div>
            {/if}
          {/each}
        {/if}
        {#if showToolData && msg.tool_data?.length}
          <ToolDataBlocks blocks={msg.tool_data} />
        {/if}
      {/if}
    </div>
  </div>
{:else}
  <!-- Continuation: gutter sits in the avatar column, text aligns under username -->
  <div class="flex gap-[0.5rem] mt-[0.5em] {extraClass}" data-msg-id={msg.id}>
    {#if timeChanged(msgs, index) && permalinkUrl}
      <a
        href={permalinkUrl}
        class="timestamp-link text-muted-foreground/70 flex-shrink-0 select-none text-[0.7rem] leading-[1.55rem] whitespace-nowrap no-underline hover:text-muted-foreground relative flex justify-end pr-[5px]"
        style="width: {AVATAR_SIZE}"
        onclick={handleTimestampClick}
      >
        {formatTimeCompact(msg.timestamp)}
        {#if copiedTooltip}
          <span class="copied-tooltip">Link copied!</span>
        {/if}
      </a>
    {:else}
      <span
        class="text-muted-foreground/70 flex-shrink-0 select-none text-[0.7rem] leading-[1.55rem] whitespace-nowrap flex justify-end pr-[5px]"
        style="width: {AVATAR_SIZE}"
      >{timeChanged(msgs, index) ? formatTimeCompact(msg.timestamp) : ''}</span>
    {/if}
    <div class="flex-1 min-w-0">
      {#if children}
        {@render children()}
      {:else}
        {#if isAction(msg)}
          <div class="flex gap-0 break-words">
            <span class="flex-shrink-0 mr-[0.3em]" style="color: {displayColor}">*</span>
            <span class="action-text flex-1 min-w-0" style="color: {displayColor}">{@html renderContent(getActionContent(msg), getApiBase())}</span>
          </div>
        {:else}
          {#each parseInsightSegments(msg.content || '') as segment}
            {#if segment.type === 'insight'}
              <div class="border-l-2 pl-3 max-w-[85%] my-0.5" style="border-color: {displayColor}80">
                <div class="message-text text-foreground">{@html renderContent(segment.content, getApiBase())}</div>
              </div>
            {:else if segment.content.trim()}
              <div class="break-words message-text {isDimSender(msg.from, dimSenders) ? 'text-muted-foreground' : 'text-foreground'}">{@html renderContent(segment.content, getApiBase())}</div>
            {/if}
          {/each}
        {/if}
        {#if showToolData && msg.tool_data?.length}
          <ToolDataBlocks blocks={msg.tool_data} />
        {/if}
      {/if}
    </div>
  </div>
{/if}

<style>
  .timestamp-link {
    cursor: pointer;
    text-decoration: none;
  }

  .timestamp-link:hover {
    text-decoration: underline;
  }

  .copied-tooltip {
    position: absolute;
    left: 50%;
    transform: translateX(-50%);
    bottom: calc(100% + 4px);
    background: hsl(var(--card));
    color: hsl(var(--foreground));
    border: 1px solid hsl(var(--border));
    font-size: 0.65rem;
    padding: 2px 8px;
    border-radius: 4px;
    white-space: nowrap;
    pointer-events: none;
    animation: tooltip-fade 1.5s ease-out forwards;
    z-index: 50;
    box-shadow: 0 2px 6px rgba(0, 0, 0, 0.15);
  }

  @keyframes tooltip-fade {
    0% { opacity: 1; }
    70% { opacity: 1; }
    100% { opacity: 0; }
  }
</style>
