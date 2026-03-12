<script>
import ReplyIcon from "@lucide/svelte/icons/reply";
import SendHorizontal from "@lucide/svelte/icons/send-horizontal";
import { onMount, tick, untrack } from "svelte";
import { fly } from "svelte/transition";
import Autocomplete from "./Autocomplete.svelte";
import { closeThread, openTaskThread, openThread, sendMessage, uploadFile } from "./api.js";
import { openImageLightbox } from "./biggerPicture.js";
import {
	collectToolBlocks,
	findPr as findPrUtil,
	getMostRecentToolCall,
	getPrUrl as getPrUrlUtil,
	hasInProgressToolBlocks,
	resolveMessageTapAction,
} from "./channelUtils.js";
import { handleCodePaste } from "./codePaste.js";
import DayDivider from "./DayDivider.svelte";
import { extractPastedFile, updatePreviewUrl, uploadAndSend } from "./filePaste.js";
import MessageRow from "./MessageRow.svelte";
import { AVENUE_COLORS, dateChanged, formatTime, getSenderColor, timeChanged } from "./messageUtils.js";
import { clearMobileTextarea } from "./mobileInput.js";
import {
	activeChannel,
	activeProject,
	channelSettings,
	channels as channelsStore,
	channelTargetMsgId,
	coworkers,
	daemonStatus,
	isWideScreen,
	kanbanData,
	messages,
	messagesByChannel,
	repoStatus,
	repoStatuses,
	threadData,
	threadUnreadCounts,
} from "./store.js";
import ToolRunSummary from "./ToolRunSummary.svelte";
import { groupToolRuns } from "./toolRunGrouping.js";

// Windowed rendering: only render a slice of messages near the viewport.
// Messages outside this window are not mounted in the DOM.
const INITIAL_WINDOW_SIZE = 100; // messages to render on first load
const LOAD_MORE_COUNT = 50; // messages to add when scrolling up

let inputText = $state("");
let scrollAreaViewport = $state(null);
let autoScroll = $state(true);
let pendingFile = $state(null);
let pendingFileUrl = $state(null);
let uploading = $state(false);
let textareaElement = $state(null);
let formWrapperElement = $state(null);
let topSentinel = $state(null);
let topObserver = null;

// The index into channelMessages where rendering begins.
// Messages before this index are not in the DOM.
let renderStartIndex = $state(0);

// Per-channel draft storage: saves inputText and pendingFile when switching channels
let channelDrafts = new Map();
let prevChannel = null;

// Autocomplete state
let showAutocomplete = $state(false);
let autocompleteType = $state(null); // '@' | '!' | '#'
let autocompleteQuery = $state("");
let autocompleteItems = $state([]);
let autocompletePosition = $state({ top: 0, left: 0 });
let autocompleteSelectedIndex = $state(0);
let autocompleteStartPos = $state(0);

// DM channel detection: use is_dm field or dm- prefix fallback
let activeChannelMeta = $derived($channelsStore.find((ch) => ch.name === $activeChannel) ?? null);
let isDm = $derived(activeChannelMeta?.is_dm ?? $activeChannel.startsWith("dm-"));
let dmPeerName = $derived($activeChannel.startsWith("dm-") ? $activeChannel.slice(3) : $activeChannel);
let showInlineToolData = $derived(isDm || ($channelSettings[$activeChannel]?.inlineToolCalls ?? true));

// Filter messages by active channel
let channelMessages = $derived($messagesByChannel[$activeChannel] || []);

// Visible slice of messages for the DOM. Only these get rendered.
let visibleMessages = $derived(channelMessages.slice(renderStartIndex));
let groupedMessages = $derived.by(() => {
	const groups = groupToolRuns(visibleMessages);
	let offset = 0;
	return groups.map((segment) => {
		const startOffset = offset;
		if (segment.type === "tool-run") {
			offset += segment.messages.length;
		} else {
			offset += 1;
		}
		return { ...segment, _offset: startOffset };
	});
});
let hasMoreAbove = $derived(renderStartIndex > 0);

// Track how many messages were present when each channel was first viewed.
// Messages at or above this index are "new" and get the slide-up animation.
// We use $state.raw so mutations don't trigger full reactive updates.
let initialMessageCounts = $state.raw({});
// Synchronous shadow: prevents re-entrant effect runs from bumping the
// snapshot count when new messages arrive before the deferred write fires.
// Without this, queueMicrotask creates a window where the guard
// `ch in initialMessageCounts` stays false, causing each new message to
// schedule another microtask with a higher len — so isNewMessage() returns
// false for genuinely new messages (the animation bug).
//
// Note: Unlike the sibling renderStartIndex effect (which uses a version
// counter to allow re-scheduling on channel switch), this effect uses a
// simple synchronous guard because we only ever need the *first* snapshot
// per channel — subsequent runs should be ignored entirely, not re-queued.
let pendingInitialCounts = {};

$effect(() => {
	// Reactive on both $activeChannel and channelMessages.length.
	// On first visit to a channel, channelMessages is empty (history not yet
	// loaded from WebSocket). We wait until messages actually arrive before
	// snapshotting the count. This prevents the race where we snapshot 0,
	// then history loads and every message animates as "new".
	const ch = $activeChannel;
	const len = channelMessages.length;
	if (!(ch in pendingInitialCounts) && len > 0) {
		pendingInitialCounts[ch] = len;
		const snapshotLen = len;
		// Defer state write to avoid state_unsafe_mutation during derived evaluation
		queueMicrotask(() => {
			initialMessageCounts = { ...initialMessageCounts, [ch]: snapshotLen };
		});
	}
});

// Position the render window at the tail on channel switch or first history load.
// Tracks $activeChannel and channelMessages.length, but uses prevRenderChannel
// to distinguish channel switches from new-message arrivals. This avoids both:
//  - stale counts (issue: window grows unbounded on revisit)
//  - DOM flash (issue: renderStartIndex starts at 0 then jumps)
let prevRenderChannel = null;
let renderVersion = 0; // version counter to discard stale microtasks
// Shadow of renderStartIndex set synchronously so the "only fires once"
// guard works even before the deferred write executes. Also updated by the
// scroll-to-message effect to prevent the guard from misfiring after a
// search navigation clears.
let pendingRenderStartIndex = 0;
$effect(() => {
	const ch = $activeChannel;
	const len = channelMessages.length;

	// If a search target is pending, skip window repositioning — the
	// scroll-to-message effect will position the window around the target.
	if (untrack(() => $channelTargetMsgId)) {
		if (ch !== prevRenderChannel) prevRenderChannel = ch;
		return;
	}

	if (ch !== prevRenderChannel) {
		// Channel switch — position at tail using current message count.
		prevRenderChannel = ch;
		const version = ++renderVersion;
		const newIndex = Math.max(0, len - INITIAL_WINDOW_SIZE);
		pendingRenderStartIndex = newIndex;
		queueMicrotask(() => {
			if (version !== renderVersion) return;
			renderStartIndex = newIndex;
		});
	} else if (len > 0 && pendingRenderStartIndex === 0 && len > INITIAL_WINDOW_SIZE) {
		// Same channel, history just loaded (was empty, now has messages).
		// Only fires once: after this, pendingRenderStartIndex > 0 so guard fails.
		const version = ++renderVersion;
		const newIndex = len - INITIAL_WINDOW_SIZE;
		pendingRenderStartIndex = newIndex;
		queueMicrotask(() => {
			if (version !== renderVersion) return;
			renderStartIndex = newIndex;
		});
	}
	// New messages on current channel: no-op. visibleMessages is an
	// open-ended slice so new messages at the end render automatically.
});

// Save/restore drafts when switching channels
$effect(() => {
	const ch = $activeChannel;
	if (prevChannel !== null && prevChannel !== ch) {
		const currentText = untrack(() => inputText);
		const currentFile = untrack(() => pendingFile);
		if (currentText.trim() || currentFile) {
			channelDrafts.set(prevChannel, { text: currentText, file: currentFile });
		} else {
			channelDrafts.delete(prevChannel);
		}
	}
	if (prevChannel !== ch) {
		const draft = channelDrafts.get(ch);
		// Defer state writes to avoid state_unsafe_mutation during derived evaluation.
		// resizeTextarea must run after the state writes so the textarea height reflects
		// the restored draft content, not the stale empty value.
		queueMicrotask(() => {
			inputText = draft?.text ?? "";
			pendingFile = draft?.file ?? null;
			tick().then(() => resizeTextarea());
		});
	}
	prevChannel = ch;
});

// Manage blob preview URL: create on file change, revoke old URL to prevent memory leaks.
$effect(() => {
	const file = pendingFile;
	pendingFileUrl = updatePreviewUrl(
		untrack(() => pendingFileUrl),
		file,
	);
	return () => {
		if (pendingFileUrl) {
			URL.revokeObjectURL(pendingFileUrl);
			pendingFileUrl = null;
		}
	};
});

function isNewMessage(channelName, index) {
	// If we haven't recorded the initial count yet (effect hasn't fired),
	// treat all messages as old so they don't animate on first render.
	const threshold = initialMessageCounts[channelName] ?? Infinity;
	return index >= threshold;
}

// Activity strip: derive tool call state from msg.tool_data on channel messages.
let allToolBlocks = $derived(collectToolBlocks(channelMessages));

// Main channel uses the top-level lead_working flag; topic channels use per-channel-lead signals.
let isLeadWorking = $derived(
	$activeChannel === $activeProject
		? !!$daemonStatus?.lead_working
		: !!$daemonStatus?.channel_leads_working?.[$activeChannel],
);

let hasInProgressItems = $derived(hasInProgressToolBlocks(allToolBlocks));

let showActivity = $derived(allToolBlocks.length > 0 || isLeadWorking);
// Use isLeadWorking as the sole dots signal. Since tool_data persists on
// messages, hasInProgressItems can be stale if an agent crashes mid-tool —
// isLeadWorking is the authoritative signal.
let showDots = $derived(isLeadWorking);

// Most recent tool call entry for inline display in the activity strip.
let mostRecentToolCallEntry = $derived.by(() => {
	const entry = getMostRecentToolCall(allToolBlocks);
	if (!entry) return null;
	// Find the original block for template compatibility
	const block = allToolBlocks.findLast((b) => b.tool_name === entry.toolName && b.call_id === entry.callId);
	return block ? { block, status: entry.status === "InProgress" ? null : entry.status } : null;
});

// Autocomplete filtering and data preparation
function getAutocompleteItems(type, query) {
	const lowerQuery = query.toLowerCase();

	if (type === "@") {
		// Coworkers + lead
		const people = [
			{ name: "lead", type: "lead" },
			...$coworkers.map((cw) => ({ name: cw.name, type: "coworker", task: cw.current_task })),
		];
		return people.filter((p) => p.name.toLowerCase().startsWith(lowerQuery));
	}

	if (type === "!") {
		// Tasks from daemon status
		const tasks = $daemonStatus?.tasks || [];
		return tasks
			.filter((t) => {
				const idMatch = String(t.id).startsWith(query);
				const subjectMatch = t.subject?.toLowerCase().startsWith(lowerQuery);
				return idMatch || subjectMatch;
			})
			.slice(0, 10); // Limit to 10 results
	}

	if (type === "#") {
		// PRs from kanban data + channels
		const prs = $kanbanData.review.map((pr) => ({
			type: "pr",
			number: pr.number,
			title: pr.title,
			status: pr.status,
		}));
		const channelList = $channelsStore.map((ch) => ({
			type: "channel",
			name: ch.name,
		}));
		const combined = [...prs, ...channelList];
		return combined
			.filter((item) => {
				if (item.type === "pr") {
					return String(item.number).startsWith(query) || item.title?.toLowerCase().startsWith(lowerQuery);
				}
				return item.name.toLowerCase().startsWith(lowerQuery);
			})
			.slice(0, 10);
	}

	return [];
}

function getAutocompleteLabel(item) {
	if (typeof item === "object" && item !== null) {
		if (item.type === "coworker" || item.type === "lead") return `@${item.name}`;
		if (item.type === "pr") return `#${item.number}`;
		if (item.type === "channel") return `#${item.name}`;
		if (item.id !== undefined) return `!${item.id}`; // task
	}
	return String(item);
}

function getAutocompleteValue(item) {
	if (typeof item === "object" && item !== null) {
		if (item.type === "coworker" || item.type === "lead") return `@${item.name}`;
		if (item.type === "pr") return `#${item.number}`;
		if (item.type === "channel") return `#${item.name}`;
		if (item.id !== undefined) return `!${item.id}`; // task
	}
	return String(item);
}

function getAutocompleteDescription(item) {
	if (typeof item === "object" && item !== null) {
		if ((item.type === "coworker" || item.type === "lead") && item.task) return item.task;
		if (item.type === "pr") return item.title;
		if (item.subject) return item.subject; // task
	}
	return null;
}

function calculateAutocompletePosition() {
	if (!textareaElement || !formWrapperElement) return { top: 0, left: 0 };

	const textareaRect = textareaElement.getBoundingClientRect();
	const wrapperRect = formWrapperElement.getBoundingClientRect();

	// Position relative to the form wrapper (which has position: relative and
	// no overflow: hidden). Using position: absolute on the dropdown instead of
	// position: fixed avoids iOS visual/layout viewport split issues when the
	// virtual keyboard is open.
	//
	// top = textarea's top edge relative to the wrapper.
	// The dropdown uses translateY(-100% - 8px) to shift above the textarea.
	return {
		top: textareaRect.top - wrapperRect.top,
		left: textareaRect.left - wrapperRect.left,
		width: textareaRect.width,
	};
}

function detectAutocompleteTrigger() {
	const cursorPos = textareaElement?.selectionStart || 0;
	// Use textarea.value directly instead of inputText binding
	// because oninput fires before the binding updates
	const text = textareaElement?.value || inputText;

	// Look backward from cursor to find trigger character
	let triggerPos = -1;
	let triggerChar = null;

	for (let i = cursorPos - 1; i >= 0; i--) {
		const char = text[i];
		const prevChar = i > 0 ? text[i - 1] : " ";

		// Check if this is a trigger character preceded by whitespace or start of line
		if ("@!#".includes(char) && (prevChar === " " || prevChar === "\n" || i === 0)) {
			triggerPos = i;
			triggerChar = char;
			break;
		}

		// Stop if we hit whitespace (no trigger found in current word)
		if (char === " " || char === "\n") {
			break;
		}
	}

	if (triggerPos >= 0 && triggerChar) {
		const query = text.slice(triggerPos + 1, cursorPos);
		autocompleteStartPos = triggerPos;
		autocompleteType = triggerChar;
		autocompleteQuery = query;
		autocompleteItems = getAutocompleteItems(triggerChar, query);
		autocompletePosition = calculateAutocompletePosition();
		autocompleteSelectedIndex = 0;
		showAutocomplete = autocompleteItems.length > 0;
	} else {
		showAutocomplete = false;
	}
}

function insertAutocompleteItem(item) {
	const value = getAutocompleteValue(item);
	const beforeTrigger = inputText.slice(0, autocompleteStartPos);
	const afterCursor = inputText.slice(textareaElement?.selectionStart || 0);

	inputText = `${beforeTrigger + value} ${afterCursor}`;
	showAutocomplete = false;

	// Set cursor position after inserted text
	tick().then(() => {
		if (textareaElement) {
			const newPos = beforeTrigger.length + value.length + 1;
			textareaElement.focus();
			textareaElement.setSelectionRange(newPos, newPos);
		}
	});
}

// Cache current tasks to avoid recalculating on every render
let currentTasks = $derived(getCurrentTasks($coworkers));

// Get PR status from kanban data
function getPrStatus(prNum) {
	const pr = $kanbanData.review.find((p) => p.number === parseInt(prNum, 10));
	return pr ? pr.status : null;
}

function findPr(prNum) {
	return findPrUtil(prNum, $kanbanData);
}

function getPrUrl(prNum) {
	return getPrUrlUtil(prNum, $kanbanData, $repoStatuses, $repoStatus.fullName);
}

// Find a task by ID from the daemon status task list
function findTask(taskId) {
	const tasks = $daemonStatus?.tasks || [];
	return tasks.find((t) => String(t.id) === String(taskId)) || null;
}

// Set up IntersectionObserver for the top sentinel (lazy load older messages)
$effect(() => {
	const sentinel = topSentinel;
	const viewport = scrollAreaViewport;
	if (!sentinel || !viewport) return;

	topObserver = new IntersectionObserver(
		(entries) => {
			for (const entry of entries) {
				if (entry.isIntersecting) {
					loadMoreMessages();
				}
			}
		},
		{ root: viewport, rootMargin: "200px 0px 0px 0px" },
	);
	topObserver.observe(sentinel);

	return () => {
		topObserver?.disconnect();
		topObserver = null;
	};
});

// Handle clicks on channel links, task links, PR links, and coworker links
onMount(() => {
	function handleLinkClick(e) {
		const target = e.target;
		if (target.classList.contains("channel-link")) {
			e.preventDefault();
			const channelName = target.dataset.channel;
			if ($channelsStore.some((ch) => ch.name === channelName)) {
				activeChannel.set(channelName);
			}
		} else if (target.classList.contains("task-link")) {
			e.preventDefault();
			const taskId = target.dataset.task;
			const task = findTask(taskId);
			if (task) {
				openTaskThread(task, task.channel || $activeChannel);
			}
		} else if (target.classList.contains("pr-link")) {
			e.preventDefault();
			const prNum = target.dataset.pr;
			const url = getPrUrl(prNum);
			if (url) window.open(url, "_blank", "noopener");
		} else if (target.classList.contains("coworker-link")) {
			// Prevent the browser from following the '#' href; no detail panel action.
			e.preventDefault();
		} else if (target.classList.contains("message-image")) {
			e.preventDefault();
			openImageLightbox(target.dataset.fullSrc || target.src);
		}
	}

	if (scrollAreaViewport) {
		scrollAreaViewport.addEventListener("click", handleLinkClick);
		return () => scrollAreaViewport.removeEventListener("click", handleLinkClick);
	}
});

// NOTE: Any new link type added to markdown.js (channel/task/PR/coworker/etc.) must be
// handled in BOTH handleLinkClick (desktop — fires on the scroll viewport) AND
// resolveMessageTapAction (mobile decision logic in channelUtils.js). handleMessageTap
// calls stopPropagation(), so handleLinkClick never runs on mobile. They are NOT
// redundant; they are two separate entry points for the same click on different platforms.
function handleMessageTap(event, msg) {
	const target = event.target instanceof Element ? event.target : null;

	// Image taps open the lightbox on all platforms (before mobile thread logic)
	if (target?.classList.contains("message-image")) {
		event.stopPropagation();
		event.preventDefault();
		openImageLightbox(target.dataset.fullSrc || target.src);
		return;
	}

	const isInteractiveControl = !!target?.closest("button, input, textarea, select, label");
	const anchor = target?.closest("a");
	const link = anchor
		? {
				isExternal: !anchor.dataset.channel && !anchor.dataset.task && !anchor.dataset.pr && !anchor.dataset.coworker,
				dataset: anchor.dataset,
			}
		: null;

	const action = resolveMessageTapAction({ isWideScreen: $isWideScreen, msg, isInteractiveControl, link });
	if (!action) return;

	if (action.type === "open_task") {
		const task = findTask(action.taskId);
		if (task) openTaskThread(task, task.channel || $activeChannel);
	} else if (action.type === "open_pr") {
		const url = getPrUrl(action.prNum);
		if (url) window.open(url, "_blank", "noopener");
	} else if (action.type === "open_thread") {
		openThread(msg, $activeChannel);
	}
	// Prevent the click from also triggering the internal link handler (handleLinkClick),
	// and prevent the browser from following href="#" which would scroll to page top.
	event.stopPropagation();
	event.preventDefault();
}

// Build a map of coworker name -> current task
function getCurrentTasks(coworkerList) {
	const map = {};
	for (const cw of coworkerList) {
		if (cw.current_task) {
			map[cw.name.toLowerCase()] = cw.current_task;
		}
	}
	return map;
}

// Auto-scroll to bottom when new messages arrive
$effect(() => {
	if (channelMessages.length > 0 && autoScroll && scrollAreaViewport) {
		// Skip auto-scroll when a search target is pending — the scroll-to-message
		// effect below will handle positioning to the right message.
		if (untrack(() => $channelTargetMsgId)) return;
		tick().then(() => {
			scrollAreaViewport.scrollTop = scrollAreaViewport.scrollHeight;
		});
	}
});

// Scroll-to-message: when channelTargetMsgId is set (e.g. from search), find the
// target message, expand the render window if needed, scroll to it, and highlight.
$effect(() => {
	const targetId = $channelTargetMsgId;
	if (!targetId || !scrollAreaViewport || channelMessages.length === 0) return;

	// Find the target message's index in channelMessages.
	// If not found yet, leave the target set — this effect re-fires reactively
	// when channelMessages changes (e.g. after fetchHistory completes).
	const targetIndex = channelMessages.findIndex((m) => m.id === targetId);
	if (targetIndex === -1) return;

	// Cancel any pending window-positioning microtask so it can't overwrite
	// the renderStartIndex we're about to set.
	const version = ++renderVersion;

	// Ensure the target is within the render window
	const needsExpansion = targetIndex < renderStartIndex;
	const newStart = needsExpansion ? Math.max(0, targetIndex - 10) : renderStartIndex;

	// Always update pendingRenderStartIndex so the window-positioning effect's
	// "only fires once" guard (pendingRenderStartIndex === 0) doesn't misfire
	// after the target is cleared and new messages arrive.
	pendingRenderStartIndex = newStart;

	// Disable auto-scroll so the auto-scroll effect doesn't fight us
	untrack(() => {
		autoScroll = false;
	});

	// Use queueMicrotask to avoid state_unsafe_mutation, then tick() for DOM.
	// Nesting tick() inside the microtask ensures it resolves AFTER the
	// renderStartIndex update is applied, so the target element exists in the DOM.
	// Guard with renderVersion so rapid re-fires only execute the latest microtask.
	queueMicrotask(() => {
		if (version !== renderVersion) return;
		if (needsExpansion) {
			renderStartIndex = newStart;
		}
		tick().then(() => {
			if (version !== renderVersion) return;
			const el = scrollAreaViewport?.querySelector(`[data-msg-id="${CSS.escape(targetId)}"]`);
			if (el) {
				el.scrollIntoView({ behavior: "smooth", block: "center" });
				el.classList.add("deep-link-highlight");
				setTimeout(() => el.classList.remove("deep-link-highlight"), 2000);
			}
			channelTargetMsgId.set(null);
		});
	});
});

// Reset textarea height when input is cleared (after send)
$effect(() => {
	inputText;
	tick().then(() => resizeTextarea());
});

async function handleSubmit(e) {
	e.preventDefault();

	// If there's a pending file, upload it first
	if (pendingFile && !uploading) {
		uploading = true;
		const result = await uploadAndSend(pendingFile, inputText, $activeChannel);
		uploading = false;

		if (result.ok) {
			inputText = "";
			if (textareaElement) textareaElement.value = "";
			pendingFile = null;
			channelDrafts.delete($activeChannel);
		} else {
			alert(`Upload failed: ${result.error}`);
			return;
		}
	} else if (inputText.trim()) {
		sendMessage(inputText.trim(), $activeChannel);
		inputText = "";
		channelDrafts.delete($activeChannel);
		clearMobileTextarea(textareaElement, () => {
			inputText = "";
		});
	}
}

function handlePaste(e) {
	const file = extractPastedFile(e);
	if (file) {
		pendingFile = file;
		return;
	}
	const cursorPos = handleCodePaste(
		e,
		textareaElement,
		() => inputText,
		(t) => {
			inputText = t;
		},
	);
	if (cursorPos !== false) {
		tick().then(() => {
			textareaElement.selectionStart = cursorPos;
			textareaElement.selectionEnd = cursorPos;
		});
	}
}

function clearPendingFile() {
	pendingFile = null;
}

function handleKeyDown(e) {
	// Handle autocomplete navigation
	if (showAutocomplete) {
		if (e.key === "ArrowDown") {
			e.preventDefault();
			autocompleteSelectedIndex = (autocompleteSelectedIndex + 1) % autocompleteItems.length;
			return;
		}
		if (e.key === "ArrowUp") {
			e.preventDefault();
			autocompleteSelectedIndex =
				autocompleteSelectedIndex === 0 ? autocompleteItems.length - 1 : autocompleteSelectedIndex - 1;
			return;
		}
		if (e.key === "Enter" || e.key === "Tab") {
			e.preventDefault();
			if (autocompleteItems[autocompleteSelectedIndex]) {
				insertAutocompleteItem(autocompleteItems[autocompleteSelectedIndex]);
			}
			return;
		}
		if (e.key === "Escape") {
			e.preventDefault();
			showAutocomplete = false;
			return;
		}
	}

	// Submit on Enter, allow Shift+Enter for new lines
	if (e.key === "Enter" && !e.shiftKey) {
		e.preventDefault();
		handleSubmit(e);
	}
}

// Load more messages when scrolling to the top of the visible window.
// Preserves scroll position so the user doesn't jump.
function loadMoreMessages() {
	if (renderStartIndex <= 0 || !scrollAreaViewport) return;
	const prevScrollHeight = scrollAreaViewport.scrollHeight;
	const prevScrollTop = scrollAreaViewport.scrollTop;
	renderStartIndex = Math.max(0, renderStartIndex - LOAD_MORE_COUNT);
	// After Svelte renders the new messages, restore scroll position
	tick().then(() => {
		if (scrollAreaViewport) {
			const newScrollHeight = scrollAreaViewport.scrollHeight;
			scrollAreaViewport.scrollTop = prevScrollTop + (newScrollHeight - prevScrollHeight);
		}
	});
}

function handleScroll() {
	if (!scrollAreaViewport) return;
	const { scrollTop, scrollHeight, clientHeight } = scrollAreaViewport;
	autoScroll = scrollHeight - scrollTop - clientHeight < 50;
}

function scrollToBottom() {
	if (scrollAreaViewport) {
		scrollAreaViewport.scrollTop = scrollAreaViewport.scrollHeight;
	}
}

function resizeTextarea() {
	if (!textareaElement) return;
	textareaElement.style.overflowY = "hidden";
	textareaElement.style.height = "auto";
	textareaElement.style.height = `${textareaElement.scrollHeight}px`;
	textareaElement.style.overflowY = textareaElement.scrollHeight > textareaElement.clientHeight ? "auto" : "hidden";
}

// Re-measure textarea height when its width changes (e.g., thread panel opens/closes,
// window resize, sidebar toggle). Track previous width to avoid infinite loops —
// without this guard, height changes from resizeTextarea() would re-trigger the observer.
$effect(() => {
	if (!textareaElement) return;
	let prevWidth = textareaElement.getBoundingClientRect().width;
	const ro = new ResizeObserver((entries) => {
		const entry = entries[0];
		if (!entry) return;
		const newWidth = entry.contentRect.width;
		if (newWidth !== prevWidth) {
			prevWidth = newWidth;
			resizeTextarea();
		}
	});
	ro.observe(textareaElement);
	return () => ro.disconnect();
});

function handleInput() {
	resizeTextarea();
	detectAutocompleteTrigger();
}

function describeToolCall(entry) {
	const block = entry.block;
	// Derive a human-readable description from the tool block's input.
	// For Bash: show the command; for file ops: show the file path; otherwise tool name.
	if (block.tool_name === "Bash" && block.input?.command) {
		const cmd = block.input.command;
		return cmd.length > 60 ? `${cmd.slice(0, 57)}...` : cmd;
	}
	if (block.input?.file_path) {
		const fp = block.input.file_path;
		const short = fp.split("/").slice(-2).join("/");
		return `${block.tool_name.toLowerCase()} ${short}`;
	}
	return block.tool_name?.toLowerCase() || "?";
}

function getToolCallStatusIcon(entry) {
	if (entry.status === "error") return "✗";
	if (entry.status === "ok") return "✓";
	return "›";
}
</script>

<div class="flex flex-col h-full min-h-0 overflow-hidden relative">
  <div
    class="flex-1 min-h-0 overflow-y-auto overflow-x-hidden overscroll-contain text-[1rem] leading-[1.55] px-[18px] pt-[14px] pb-[18px]"
    bind:this={scrollAreaViewport}
    onscroll={handleScroll}
  >
      {#if channelMessages.length === 0}
        <div class="text-center text-muted-foreground py-[50px] px-[22px] font-sans">
          <p>No messages {isDm ? `with @${dmPeerName}` : `in #${$activeChannel}`}</p>
          <p class="text-[0.9rem] mt-[10px]">{isDm ? `Send a message to start a conversation` : `Messages posted to this channel will appear here`}</p>
        </div>
      {:else}
        {#if hasMoreAbove}
          <!-- Top sentinel: triggers loading older messages when scrolled into view.
               Only mounted when there are messages above the window, so the
               IntersectionObserver doesn't fire no-op callbacks on short channels. -->
          <div bind:this={topSentinel} class="h-[1px] w-full" aria-hidden="true"></div>
          <div class="text-center text-muted-foreground/50 text-[0.8rem] py-2 select-none">Loading earlier messages…</div>
        {/if}

        {#each groupedMessages as segment}
          {#if segment.type === 'tool-run'}
            <ToolRunSummary
              messages={segment.messages}
              toolCount={segment.toolCount}
              lastTimestamp={segment.lastTimestamp}
              allMessages={channelMessages}
              startIndex={renderStartIndex + segment._offset}
              channelName={$activeChannel}
              {currentTasks}
              showToolData={showInlineToolData}
            />
          {:else}
            {@const msg = segment.message}
            {@const globalIndex = renderStartIndex + segment._offset}
            {@const dayLabel = dateChanged(channelMessages, globalIndex)}
            {#if dayLabel}
              <DayDivider label={dayLabel} />
            {/if}
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <div
              data-testid="message-row"
              in:fly={{ y: 16, duration: isNewMessage($activeChannel, globalIndex) ? 180 : 0, opacity: 0 }}
              class="group relative -mx-[18px] px-[18px] pb-[5px] rounded-sm hover:bg-accent/30"
              class:auto-output={msg.auto_output}
              class:opacity-60={msg.pending}
              class:mobile-thread-tappable={!$isWideScreen && !msg.thread_parent_id}
              onclick={(event) => handleMessageTap(event, msg)}
            >
            {#if !msg.thread_parent_id}
              <button
                data-testid="thread-reply-button"
                class="hidden lg:flex absolute right-6 -top-3.5 items-center gap-2 px-3.5 py-1.5 rounded-lg border border-border bg-card text-[0.85rem] font-bold text-foreground cursor-pointer opacity-0 pointer-events-none transition-all duration-150 shadow-sm group-hover:opacity-100 group-hover:pointer-events-auto focus:opacity-100 focus:pointer-events-auto hover:border-primary hover:shadow-md"
                onclick={() => openThread(msg, $activeChannel)}
                aria-label="Reply in thread"
              >
                <ReplyIcon size={16} />
                <span>Reply</span>
              </button>
            {/if}

            <MessageRow
              {msg}
              msgs={channelMessages}
              index={globalIndex}
              senderClass="mt-1"
              currentTask={currentTasks[msg.from.toLowerCase()]}
              channelName={$activeChannel}
              showToolData={showInlineToolData}
            />

            <!-- Reply indicator for messages with thread replies -->
            {#if !msg.thread_parent_id && msg.reply_count}
              {@const threadUnread = $threadUnreadCounts[msg.id] || 0}
              {@const participants = msg.reply_participants || (msg.last_reply ? [msg.last_reply.from] : [])}
              <div class="flex gap-0" style="padding-left: calc(2.4rem + 0.5rem);">
                <button
                  data-testid="thread-summary"
                  class="flex items-center gap-1.5 text-[0.75rem] text-link-default hover:text-link-hover cursor-pointer bg-transparent border-none p-0 mt-0.5"
                  onclick={() => openThread(msg, $activeChannel)}
                >
                  {#if participants.length > 0}
                    <span class="thread-avatars">
                      {#each participants as p}
                        <span
                          class="thread-avatar-chip"
                          style="background-color: {getSenderColor(p)}"
                          title={p}
                        >{p[0].toUpperCase()}</span>
                      {/each}
                    </span>
                  {/if}
                  {#if threadUnread > 0}
                    <span class="thread-unread-pill">{threadUnread} new</span>
                  {:else}
                    <span>{msg.reply_count} {msg.reply_count === 1 ? 'reply' : 'replies'}</span>
                  {/if}
                  {#if msg.last_reply}
                    <span class="text-muted-foreground/60">&middot;</span>
                    <span class="text-muted-foreground">{formatTime(msg.last_reply.timestamp)}</span>
                  {/if}
                </button>
              </div>
            {/if}
            </div>
          {/if}
        {/each}
      {/if}

  </div>

  <!-- Activity strip: always rendered at fixed height to prevent layout shift.
       Shows [dots?] [lead name] [icon] [tool description] on one line.
       Dots are driven by lead_working for the main channel and channel_leads_working
       for topic channels (same 5s activity-timeout signal as the TUI braille spinner). -->
  <div class="h-[1.5em] flex items-center gap-[6px] px-[18px] text-[0.82rem] overflow-hidden whitespace-nowrap shrink-0" data-testid="activity-strip">
    {#if showActivity}
      {#if showDots}
        <span class="typing-dots flex gap-[3px] items-center">
          <span class="dot w-[5px] h-[5px] rounded-full" style="background-color: {AVENUE_COLORS.lead}"></span>
          <span class="dot w-[5px] h-[5px] rounded-full" style="background-color: {AVENUE_COLORS.lead}"></span>
          <span class="dot w-[5px] h-[5px] rounded-full" style="background-color: {AVENUE_COLORS.lead}"></span>
        </span>
      {/if}
      <span class="font-mono font-semibold" style="color: {AVENUE_COLORS.lead}">{isDm ? `@${dmPeerName}` : $activeChannel}</span>
      {#if mostRecentToolCallEntry}
        <span class="text-muted-foreground/60 select-none">{getToolCallStatusIcon(mostRecentToolCallEntry)}</span>
        <span class="font-mono text-muted-foreground truncate">{describeToolCall(mostRecentToolCallEntry)}</span>
      {/if}
    {/if}
  </div>

  {#if !autoScroll}
    <button
      class="absolute bottom-[90px] right-[22px] w-[40px] h-[40px] rounded-full border-2 border-border bg-card text-foreground text-[1.2rem] cursor-pointer flex items-center justify-center transition-all duration-200 opacity-85 hover:opacity-100 hover:border-primary hover:text-primary z-10"
      onclick={scrollToBottom}
      aria-label="Scroll to bottom"
    >
      &#8595;
    </button>
  {/if}

  <!-- Input area wrapper: position: relative so the autocomplete can be
       absolutely positioned above the textarea without being clipped by the
       Channel root's overflow: hidden. The autocomplete uses translateY(-100% - 8px)
       to shift above the textarea's top edge. -->
  <div class="relative shrink-0" bind:this={formWrapperElement}>
    <!-- Autocomplete dropdown — positioned absolute within this wrapper -->
    <Autocomplete
      bind:show={showAutocomplete}
      bind:selectedIndex={autocompleteSelectedIndex}
      items={autocompleteItems}
      position={autocompletePosition}
      getLabel={getAutocompleteLabel}
      getValue={getAutocompleteValue}
      getDescription={getAutocompleteDescription}
      onSelect={insertAutocompleteItem}
    />
    <form class="flex flex-col gap-2 px-3 py-1.5 bg-card border-t border-border" onsubmit={handleSubmit}>
      {#if pendingFile}
        <div class="relative inline-block max-w-[200px] border border-border rounded-lg p-2 bg-card" data-testid="file-preview">
          {#if pendingFile.type.startsWith('image/')}
            <img src={pendingFileUrl} alt="Preview" class="max-w-full max-h-[120px] rounded block" />
          {:else}
            <div class="flex items-center gap-2 text-foreground">
              <span class="text-[1.5rem]">&#128196;</span>
              <span class="text-[0.85rem] overflow-hidden text-ellipsis whitespace-nowrap">{pendingFile.name}</span>
            </div>
          {/if}
          <button
            type="button"
            class="absolute top-1 right-1 w-6 h-6 p-0 rounded-full bg-[rgba(0,0,0,0.7)] text-white text-[1.2rem] leading-none flex items-center justify-center cursor-pointer border border-border hover:bg-[rgba(255,87,87,0.8)] hover:border-destructive"
            onclick={clearPendingFile}
            aria-label="Remove file"
          >
            &times;
          </button>
        </div>
      {/if}
      <div class="relative w-full">
        <textarea
          data-testid="channel-input"
          bind:this={textareaElement}
          bind:value={inputText}
          placeholder={isDm ? `Message @${dmPeerName}...` : `Message to #${$activeChannel}...`}
          rows="1"
          class="block w-full py-[13px] px-[17px] pr-[48px] border-2 border-border rounded-[18px] bg-background text-foreground text-[1.02rem] font-inherit outline-none resize-none min-h-[1.6em] max-h-[50vh] overflow-y-hidden focus:border-primary placeholder:text-muted-foreground"
          onkeydown={handleKeyDown}
          onpaste={handlePaste}
          oninput={handleInput}
        ></textarea>
        <button
          type="submit"
          disabled={(!inputText.trim() && !pendingFile) || uploading}
          data-testid="send-button"
          class="absolute right-[12px] bottom-[10px] p-1.5 rounded-full border-none bg-primary text-primary-foreground cursor-pointer transition-all duration-200 disabled:opacity-30 disabled:cursor-not-allowed hover:bg-primary/90"
        >
          <SendHorizontal size={18} />
        </button>
      </div>
    </form>
  </div>
</div>

<style>
  /* Inline image attachments — thumbnail with lightbox on click */
  :global(.message-image) {
    max-width: 200px;
    max-height: 200px;
    border-radius: 6px;
    display: block;
    margin-top: 4px;
    cursor: pointer;
    transition: opacity 0.15s;
  }

  :global(.message-image:hover) {
    opacity: 0.85;
  }

  :global(.attachment-ref) {
    display: inline-block;
    font-size: 0.88em;
    color: hsl(var(--muted-foreground));
    background: hsl(var(--accent));
    border-radius: 4px;
    padding: 2px 8px;
    margin-top: 4px;
  }

  /* Link styles - applied globally within message content */
  :global(.message-text a),
  :global(.action-text a) {
    color: hsl(var(--link-default));
    text-decoration: none;
  }

  :global(.message-text a:hover),
  :global(.action-text a:hover) {
    text-decoration: underline;
  }

  :global(.message-text a.channel-link),
  :global(.action-text a.channel-link) {
    color: hsl(var(--link-default));
    font-weight: 600;
    cursor: pointer;
  }

  :global(.message-text a.task-link),
  :global(.action-text a.task-link) {
    color: hsl(var(--link-task));
    font-weight: 600;
    cursor: pointer;
  }

  :global(.message-text a.pr-link),
  :global(.action-text a.pr-link) {
    color: hsl(var(--link-pr));
    font-weight: 600;
    cursor: pointer;
  }

  :global(.message-text a.coworker-link),
  :global(.action-text a.coworker-link) {
    color: hsl(var(--link-coworker));
    font-weight: 600;
    cursor: pointer;
  }

  /* Inline code */
  :global(.message-text code),
  :global(.action-text code) {
    background: hsl(var(--accent));
    padding: 0.12em 0.45em;
    border-radius: 3px;
    font-size: 0.92em;
    font-family: 'IBM Plex Mono', 'SF Mono', 'Menlo', monospace;
  }

  /* Code blocks */
  :global(.message-text pre),
  :global(.action-text pre) {
    background: hsl(var(--accent));
    padding: 10px 14px;
    border-radius: 5px;
    overflow-x: auto;
    margin: 5px 0;
    border: none;
  }

  :global(.message-text pre code),
  :global(.action-text pre code) {
    background: none !important;
    padding: 0 !important;
    border: none;
    border-radius: 0;
    font-size: 0.88em;
  }

  /* Headings - scaled down for chat context */
  :global(.message-text h1),
  :global(.message-text h2),
  :global(.message-text h3),
  :global(.action-text h1),
  :global(.action-text h2),
  :global(.action-text h3) {
    font-size: 1em;
    font-weight: 700;
    margin: 5px 0 3px;
  }

  /* Lists — restore list-style-type stripped by Tailwind Preflight */
  :global(.message-text ul),
  :global(.action-text ul) {
    margin: 3px 0;
    padding-left: 1.6em;
    list-style-type: disc;
  }

  :global(.message-text ol),
  :global(.action-text ol) {
    margin: 3px 0;
    padding-left: 1.6em;
    list-style-type: decimal;
  }

  /* Blockquotes */
  :global(.message-text blockquote),
  :global(.action-text blockquote) {
    border-left: 2px solid hsl(var(--border));
    margin: 3px 0;
    padding-left: 10px;
    color: hsl(var(--muted-foreground));
  }

  /* Tables */
  :global(.message-text table),
  :global(.action-text table) {
    border-collapse: collapse;
    margin: 6px 0;
    font-size: 0.88em;
  }

  :global(.message-text th),
  :global(.action-text th) {
    background: hsl(var(--accent));
    color: hsl(var(--primary));
    padding: 4px 10px;
    text-align: left;
    border: 1px solid hsl(var(--border));
    font-weight: 600;
  }

  :global(.message-text td),
  :global(.action-text td) {
    padding: 3px 10px;
    border: 1px solid hsl(var(--border));
    color: hsl(var(--foreground));
  }

  :global(.message-text tr:nth-child(even) td),
  :global(.action-text tr:nth-child(even) td) {
    background: hsl(var(--secondary));
  }

  /* Paragraph spacing within messages */
  :global(.message-text p + p),
  :global(.action-text p + p) {
    margin-top: 0.5em;
  }

  /* Typing indicator bounce animation */
  @keyframes typing-bounce {
    0%, 80%, 100% {
      opacity: 0.3;
      transform: scale(0.8);
    }
    40% {
      opacity: 1;
      transform: scale(1);
    }
  }

  :global(.typing-dots .dot) {
    animation: typing-bounce 1.4s infinite ease-in-out both;
  }

  :global(.typing-dots .dot:nth-child(1)) {
    animation-delay: 0s;
  }

  :global(.typing-dots .dot:nth-child(2)) {
    animation-delay: 0.2s;
  }

  :global(.typing-dots .dot:nth-child(3)) {
    animation-delay: 0.4s;
  }

  @media (max-width: 1023px) {
    .mobile-thread-tappable {
      cursor: pointer;
    }
  }

  .thread-unread-pill {
    padding: 1px 5px;
    border-radius: 8px;
    background: hsl(var(--accent-teal));
    color: white;
    font-size: 0.6rem;
    font-weight: 700;
    line-height: 1.2;
  }

  .thread-avatars {
    display: inline-flex;
    flex-shrink: 0;
  }

  .thread-avatar-chip {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    border-radius: 3px;
    font-size: 0.5rem;
    font-weight: 700;
    color: white;
    line-height: 1;
    flex-shrink: 0;
    margin-right: -3px;
    outline: 1.5px solid hsl(var(--background));
  }

  .thread-avatar-chip:last-child {
    margin-right: 0;
  }

  /* Search result scroll-to-message highlight animation */
  :global(.deep-link-highlight) {
    animation: deep-link-flash 2s ease-out;
  }

  @keyframes deep-link-flash {
    0% { background-color: hsl(var(--primary) / 0.2); }
    70% { background-color: hsl(var(--primary) / 0.2); }
    100% { background-color: transparent; }
  }

</style>
