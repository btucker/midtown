<script lang="ts">
import CircleCheck from "@lucide/svelte/icons/circle-check";
import CircleX from "@lucide/svelte/icons/circle-x";
import Feather from "@lucide/svelte/icons/feather";
import Github from "@lucide/svelte/icons/github";
import LoaderCircle from "@lucide/svelte/icons/loader-circle";
import Search from "@lucide/svelte/icons/search";
import { openTaskThread, selectDm } from "./api.ts";
import { getPrUrl as getPrUrlUtil } from "./channelUtils.ts";
import DynamicIcon from "./DynamicIcon.svelte";
import { renderContent } from "./markdown.ts";
import { getSenderColor } from "./messageUtils.ts";
import {
	activeChannel,
	channels,
	coworkerMap as coworkerMapStore,
	daemonStatus,
	kanbanData,
	repoStatus,
	repoStatuses,
} from "./store.ts";
import { rolledUpStatus as computeRolledUpStatus, statusBarColor } from "./taskStatus.ts";

let {
	task,
	cw = null,
	children = [],
	reviewer = null,
	reviewPosted = false,
	onclick = null,
	variant = "row",
	channelLabel = "",
} = $props();

const isCard = $derived(variant === "card");
// Status rollup: in_progress if any child is in_progress; completed only when all are completed
const rolledUpStatus = $derived(computeRolledUpStatus(task, children, isCard));
const isActive = $derived(rolledUpStatus === "in_progress");
const isBlocked = $derived(task.blocked_by?.length > 0);

// Use shared store-level coworkerMap (avoids per-instance Map creation)
const cwMap = $derived(isCard || children.length > 0 ? $coworkerMapStore : null);
const relatedPr = $derived(isCard ? $kanbanData.review.find((pr) => String(pr.task_id) === String(task.id)) : null);
const effectiveCw = $derived(isCard ? (task.owner ? (cwMap?.get(task.owner) ?? null) : null) : cw);
const effectiveReviewer = $derived(isCard ? (relatedPr?.reviewer ?? null) : reviewer);
const effectiveReviewPosted = $derived(isCard ? relatedPr?.review_posted || false : reviewPosted);
const hasProgress = $derived(effectiveCw?.progress != null);
const ownerColor = $derived(task.color || effectiveCw?.color || (task.owner ? getSenderColor(task.owner) : null));
const ownerIcon = $derived(task.icon || effectiveCw?.icon);

const prUrl = $derived(
	relatedPr && $repoStatus.fullName ? `https://github.com/${$repoStatus.fullName}/pull/${relatedPr.number}` : null,
);
const descriptionHtml = $derived(isCard && task.description ? renderContent(task.description) : "");

function lifecycleSegments(cwProgress, reviewer, reviewPosted, ownerColor, reviewerColor) {
	const segments = [];
	if (!reviewer) {
		segments.push({ width: (cwProgress / 100) * 70, color: ownerColor });
	} else if (!reviewPosted) {
		segments.push({ width: 70, color: ownerColor });
		segments.push({ width: 20, color: reviewerColor });
	} else {
		segments.push({ width: 70, color: ownerColor });
		segments.push({ width: 20, color: reviewerColor });
	}
	return segments;
}

function childSegments(children, cwMap) {
	const sliceWidth = 100 / children.length;
	return children.map((child) => {
		const childCw = child.owner ? cwMap?.get(child.owner) : null;
		const color =
			child.color || childCw?.color || (child.owner ? getSenderColor(child.owner) : "hsl(var(--muted-foreground))");
		let fill = 0;
		if (child.status === "completed") {
			fill = 1;
		} else if (child.status === "in_progress") {
			fill = childCw?.progress != null ? childCw.progress / 100 : 0.5;
		} else {
			fill = 0;
		}
		return { width: sliceWidth * fill, maxWidth: sliceWidth, color };
	});
}

function handleDescriptionClick(e) {
	const target = e.target;
	if (target.classList.contains("channel-link")) {
		e.preventDefault();
		const name = target.dataset.channel;
		if ($channels.some((ch) => ch.name === name)) $activeChannel = name;
	} else if (target.classList.contains("task-link")) {
		e.preventDefault();
		const taskId = target.dataset.task;
		const tasks = $daemonStatus?.tasks || [];
		const found = tasks.find((t) => String(t.id) === String(taskId));
		if (found) openTaskThread(found, found.channel || $activeChannel);
	} else if (target.classList.contains("pr-link")) {
		e.preventDefault();
		const prNum = target.dataset.pr;
		const url = getPrUrlUtil(prNum, $kanbanData, $repoStatuses, $repoStatus.fullName);
		if (url) window.open(url, "_blank", "noopener");
	} else if (target.classList.contains("coworker-link")) {
		e.preventDefault();
	}
}
</script>

<button
  class="task-row w-full overflow-visible flex items-stretch gap-1.5 py-[5px] cursor-pointer transition-[background] duration-100 text-left font-mono text-[0.72rem] leading-[1.3] text-muted-foreground {isCard ? 'border border-[hsl(var(--border))] bg-[hsl(var(--card))] mb-2 hover:bg-[hsl(var(--accent)_/_0.3)] pr-3 rounded-md' : 'border-none bg-transparent rounded-[5px] hover:bg-sidebar-accent'} {isActive ? 'text-sidebar-foreground' : ''} {isBlocked ? 'opacity-65' : ''}"
  onclick={isCard ? undefined : onclick}
  data-testid={isCard ? 'task-card' : undefined}
>
  {#if isCard}<span class="w-[3px] rounded-sm shrink-0 self-stretch" style="background: {statusBarColor(task.status, task.owner, ownerColor)}"></span>{/if}
  <div class="flex-1 min-w-0 flex flex-col gap-[3px]">
    {#if isCard}
      <span class="shrink-0 font-semibold text-[0.65rem] {isActive ? 'opacity-80' : 'opacity-60'}">!{task.id}</span>
      <span>{#if task.status === 'completed'}<span class="text-[hsl(var(--accent-green))]">✓ </span>{/if}{task.subject}</span>
    {:else}
      <div class="flex items-center gap-1">
        <span class="flex-1 min-w-0 overflow-hidden text-ellipsis whitespace-nowrap">{#if rolledUpStatus === 'completed'}<span class="text-[hsl(var(--accent-green))]">✓ </span>{/if}{task.subject}</span>
        {#if isBlocked}
          <span class="shrink-0 text-[0.62rem] text-[hsl(var(--status-amber))] opacity-85" title="Blocked by !{task.blocked_by[0]}">⧗ !{task.blocked_by[0]}</span>
        {/if}
        {#if channelLabel}
          <span class="shrink-0 rounded px-1 py-px text-[9px] font-mono text-muted-foreground bg-sidebar-accent">#{channelLabel}</span>
        {/if}
      </div>
    {/if}
    {#if !isCard && children.length > 0}
      {@const segments = childSegments(children, cwMap)}
      {@const doneCount = children.filter((c) => c.status === "completed").length}
      <div class="flex items-center gap-1.5 pr-0.5">
        <div class="flex-1 h-[3px] bg-sidebar-accent rounded-sm overflow-hidden flex">
          {#each segments as seg}
            <div
              class="h-full transition-[width] duration-500 ease-in-out"
              style="width: {seg.width}%; max-width: {seg.maxWidth}%; background: {seg.color}"
            ></div>
            {#if seg.maxWidth > seg.width}
              <div style="width: {seg.maxWidth - seg.width}%"></div>
            {/if}
          {/each}
        </div>
        <span class="shrink-0 text-[0.6rem] text-[hsl(var(--accent-teal))] tabular-nums">{doneCount}/{children.length}</span>
      </div>
    {:else if isActive && (hasProgress || effectiveReviewer) && task.owner}
      {@const segments = lifecycleSegments(effectiveCw?.progress ?? 0, effectiveReviewer, effectiveReviewPosted, ownerColor || getSenderColor(task.owner), effectiveReviewer ? getSenderColor(effectiveReviewer) : null)}
      {@const totalPct = Math.round(segments.reduce((sum, s) => sum + s.width, 0))}
      <div class="flex items-center gap-1.5 pr-0.5">
        <div class="flex-1 h-[3px] bg-sidebar-accent rounded-sm overflow-hidden flex">
          {#each segments as seg}
            <div
              class="h-full transition-[width] duration-500 ease-in-out"
              style="width: {seg.width}%; background: {seg.color}"
            ></div>
          {/each}
        </div>
        <span class="shrink-0 text-[0.6rem] text-[hsl(var(--accent-teal))] tabular-nums">{totalPct}%</span>
      </div>
    {/if}

    {#if isCard && (task.owner || effectiveReviewer || (relatedPr && prUrl))}
      <div class="flex items-center gap-1.5 pt-0.5">
        {#if relatedPr && prUrl}
          <a
            href={prUrl}
            target="_blank"
            rel="noopener"
            class="inline-flex items-center gap-1 text-[hsl(var(--link-default))] text-[0.72rem] no-underline hover:underline"
          ><span class="inline-flex items-center justify-center size-4 rounded-full bg-[hsl(var(--muted))]"><Github size={10} /></span>PR #{relatedPr.number}</a>
          {#if relatedPr.ci_status === 'passed'}
            <CircleCheck size={13} class="text-[hsl(var(--accent-green,145_40%_38%))]" />
          {:else if relatedPr.ci_status === 'failed'}
            <CircleX size={13} class="text-[hsl(var(--status-red,0_84%_60%))]" />
          {:else if relatedPr.ci_status === 'running'}
            <LoaderCircle size={13} class="text-[hsl(var(--status-amber,45_93%_47%))] animate-spin" />
          {/if}
        {/if}
        <span class="flex-1"></span>
        {#if task.owner}
          {@const ownerGlow = isActive && (!effectiveReviewer || effectiveReviewPosted)}
          <span
            role="button"
            tabindex="0"
            class="relative shrink-0 size-4 rounded-[3px] border-none p-0 m-0 flex items-center justify-center text-[0.55rem] font-bold text-white leading-none cursor-pointer hover:opacity-85 {ownerGlow ? 'shadow-[0_0_6px_1px_var(--glow-color)]' : ''}"
            style="background-color: {ownerColor || getSenderColor(task.owner)}; font-family: var(--font-sans){ownerGlow ? `; --glow-color: ${ownerColor || getSenderColor(task.owner)}` : ''}"
            title="{task.owner}{effectiveCw?.phase ? ` · ${effectiveCw.phase}` : ''}"
            onclick={(e) => { e.stopPropagation(); selectDm(task.owner) }}
            onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); e.stopPropagation(); selectDm(task.owner) } }}
          >{#if ownerIcon}<DynamicIcon name={ownerIcon} size={10}>{#snippet fallback()}{task.owner[0].toUpperCase()}{/snippet}</DynamicIcon>{:else}{task.owner[0].toUpperCase()}{/if}<span class="absolute -bottom-1 -right-1 flex items-center justify-center text-sidebar-foreground"><Feather size={11} strokeWidth={2.5} fill="hsl(var(--sidebar-background))" /></span></span>
        {/if}
        {#if effectiveReviewer}
          {@const reviewerGlow = isActive && !effectiveReviewPosted}
          <span
            role="button"
            tabindex="0"
            class="relative shrink-0 size-4 rounded-[3px] border-none p-0 m-0 flex items-center justify-center text-[0.55rem] font-bold text-white leading-none cursor-pointer hover:opacity-85 {reviewerGlow ? 'shadow-[0_0_6px_1px_var(--glow-color)]' : ''}"
            style="background-color: {getSenderColor(effectiveReviewer)}; font-family: var(--font-sans){reviewerGlow ? `; --glow-color: ${getSenderColor(effectiveReviewer)}` : ''}"
            title="{effectiveReviewer} · {effectiveReviewPosted ? 'reviewed' : 'reviewing'}"
            onclick={(e) => { e.stopPropagation(); selectDm(effectiveReviewer) }}
            onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); e.stopPropagation(); selectDm(effectiveReviewer) } }}
          >{effectiveReviewer[0].toUpperCase()}<span class="absolute -bottom-1 -right-1 flex items-center justify-center text-sidebar-foreground"><Search size={11} strokeWidth={2.5} fill="hsl(var(--sidebar-background))" style="transform: scaleX(-1)" /></span></span>
        {/if}
      </div>
    {/if}

    {#if isCard && task.plan}
      <div class="flex items-center gap-1 pt-0.5 text-[0.68rem] text-muted-foreground/50">
        <span title={task.plan}>📋 Plan: <span class="font-mono">{task.plan.split('/').slice(-1)[0]}</span></span>
      </div>
    {/if}
    {#if isCard && task.description}
      <details class="group pt-0.5 pb-1">
        <summary class="text-[0.72rem] text-muted-foreground/60 cursor-pointer select-none list-none flex items-center gap-1" onclick={(e) => e.stopPropagation()}>
          <span class="inline-block transition-transform group-open:rotate-90">▶</span>
          <span>Description</span>
        </summary>
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="task-description mt-1.5 text-[0.78rem] leading-[1.5] text-muted-foreground" onclick={handleDescriptionClick}>
          {@html descriptionHtml}
        </div>
      </details>
    {/if}
  </div>
</button>

<style>
  /* Scoped :global styles for rendered markdown inside {@html} — can't use Tailwind for injected HTML */
  .task-description :global(p) { margin: 0.3em 0; }
  .task-description :global(p:first-child) { margin-top: 0; }
  .task-description :global(p:last-child) { margin-bottom: 0; }
  .task-description :global(ul),
  .task-description :global(ol) { margin: 0.3em 0; padding-left: 1.5em; }
  .task-description :global(li) { margin: 0.15em 0; }
  .task-description :global(code) { font-size: 0.85em; background: hsl(var(--muted)); padding: 0.1em 0.35em; border-radius: 3px; }
  .task-description :global(pre) { margin: 0.5em 0; padding: 0.5em; border-radius: 4px; background: hsl(var(--muted)); overflow-x: auto; }
  .task-description :global(pre code) { background: none; padding: 0; }
  .task-description :global(a) { color: hsl(var(--link-default)); }
  .task-description :global(a.task-link) { color: hsl(var(--link-task)); font-weight: 600; cursor: pointer; }
  .task-description :global(a.pr-link) { color: hsl(var(--link-pr)); font-weight: 600; cursor: pointer; }
  .task-description :global(a.channel-link) { color: hsl(var(--link-default)); font-weight: 600; cursor: pointer; }
  .task-description :global(a.coworker-link) { color: hsl(var(--link-coworker)); font-weight: 600; cursor: pointer; }
  .task-description :global(strong) { color: hsl(var(--foreground)); font-weight: 600; }
  .task-description :global(blockquote) { border-left: 3px solid hsl(var(--border)); margin: 0.3em 0; padding-left: 0.75em; color: hsl(var(--muted-foreground) / 0.8); }
</style>
