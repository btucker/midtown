import { get } from "svelte/store";
import {
	activeChannel,
	activeProject,
	authProfiles,
	authProfilesByProvider,
	authSwitching,
	channels,
	connected,
	coworkers,
	daemonStatus,
	deepLinkMsgId,
	kanbanData,
	maxInProgressTasks,
	messages,
	messagesByChannel,
	openThreads,
	pendingQuestions,
	projects,
	repoStatus,
	repoStatuses,
	showArchivedChannels,
	threadData,
	threadForkOwners,
	threadForkParents,
	threadOwnership,
	threadUnreadCounts,
	trackedThreads,
	usageData,
	userSenderName,
} from "./store.ts";
import { isToolOnly } from "./toolRunGrouping.ts";
import type {
	AuthProfile,
	Channel,
	Coworker,
	DaemonStatus,
	Message,
	PendingQuestion,
	Project,
	SearchResponse,
	Task,
} from "./types.ts";

// Strip markdown from the first non-empty line of message content.
function extractPlainText(content: string | null | undefined): string {
	if (!content) return "";
	const firstLine = content.split("\n").find((l) => l.trim().length > 0) || "";
	return firstLine
		.replace(/^#{1,6}\s+/, "")
		.replace(/\*\*(.+?)\*\*/g, "$1")
		.replace(/\*(.+?)\*/g, "$1")
		.replace(/`(.+?)`/g, "$1")
		.replace(/\[(.+?)\]\(.+?\)/g, "$1")
		.trim();
}

// Extract a short subject line from message content for sidebar thread labels.
function extractThreadSubject(content: string | null | undefined): string {
	const plain = extractPlainText(content);
	if (!plain) return "Thread";
	return plain.length > 60 ? `${plain.slice(0, 57)}...` : plain;
}

// Track a thread in the sidebar.
// When the thread is already tracked, preserves lastActivity (avoids re-sorting)
// and only upgrades the subject if a better one is available.
function trackThread(
	threadParentId: string,
	channelName: string,
	content: string | null | undefined,
	opts?: { replyCount?: number; replyContent?: string | null },
): void {
	const newSubject = extractThreadSubject(content);
	// Use reply content for fullText when available, otherwise fall back to parent content
	const newFullText = extractPlainText(opts?.replyContent ?? content);
	trackedThreads.update((tracked) => {
		const existing = tracked[threadParentId];
		// Keep existing subject if the new one is just the fallback "Thread"
		const subject = newSubject !== "Thread" ? newSubject : existing?.subject || newSubject;
		// Keep existing fullText if the new one is empty
		const fullText = newFullText || existing?.fullText || "";
		return {
			...tracked,
			[threadParentId]: {
				channelName,
				subject,
				fullText,
				// Only set lastActivity on initial tracking — WS handler updates it on new replies
				lastActivity: existing?.lastActivity || new Date().toISOString(),
				replyCount: opts?.replyCount ?? (existing?.replyCount || 0),
			},
		};
	});
}

// Dismiss a tracked thread — removes from sidebar.
export function dismissThread(threadParentId: string): void {
	trackedThreads.update((tracked) => {
		const next = { ...tracked };
		delete next[threadParentId];
		return next;
	});
	threadUnreadCounts.update((counts) => {
		const next = { ...counts };
		delete next[threadParentId];
		return next;
	});
}

let ws: WebSocket | null = null;
let reconnectTimeout: ReturnType<typeof setTimeout> | null = null;
let statusPollInterval: ReturnType<typeof setInterval> | null = null;
let usagePollInterval: ReturnType<typeof setInterval> | null = null;

// Unique key for the bulk (all-channels) fetchHistory request. Using a Symbol
// avoids collisions with real channel names in the AbortController map.
const BULK_FETCH_KEY = Symbol("bulk-fetch");

// AbortController for in-flight fetchHistory requests, keyed by channel name.
// When a new fetch starts for a channel, any previous in-flight fetch for that
// channel is aborted to prevent out-of-order responses from stale requests.
const fetchHistoryControllers = new Map<string | symbol, AbortController>();

// AbortController for in-flight fetchChannelAgentsMd requests, keyed by channel name.
const fetchAgentsMdControllers = new Map<string, AbortController>();

// ── Browser history navigation ──────────────────────────────────────────────
// Tracks whether we're currently handling a popstate event to prevent
// circular history pushes (popstate → store change → pushState).
let _handlingPopstate = false;

// Base URL for the current project's daemon API.
// Always connects via the project's webhook port.
let projectApiBase = "";

const WEBSERVER_API = "/api";

// Fetch the list of projects from the shared webserver
export async function fetchProjects(): Promise<Project[]> {
	try {
		const res = await fetch(`${WEBSERVER_API}/projects`);
		if (res.ok) {
			const data = await res.json();
			projects.set(data);
			return data;
		}
	} catch (err) {
		console.error("Failed to fetch projects:", err);
	}
	return [];
}

// Fetch the set of open thread IDs for a channel
export async function fetchOpenThreads(channel: string): Promise<string[]> {
	try {
		const res = await fetch(`${getApiBase()}/channels/${encodeURIComponent(channel)}/open-threads`);
		if (res.ok) {
			const data = await res.json();
			return data.threads || [];
		}
	} catch (err) {
		console.warn("Failed to fetch open threads:", err);
	}
	return [];
}

// Persist the set of open thread IDs for a channel
export async function setOpenThreads(channel: string, threads: string[]): Promise<void> {
	try {
		await fetch(`${getApiBase()}/channels/${encodeURIComponent(channel)}/open-threads`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ threads }),
		});
	} catch (err) {
		console.warn("Failed to set open threads:", err);
	}
}

const TWELVE_HOURS_MS = 12 * 60 * 60 * 1000;
const AUTO_CLOSE_INTERVAL_MS = 5 * 60 * 1000;

// Auto-close threads with no activity for 12+ hours
setInterval(() => {
	const now = Date.now();
	const tracked = get(trackedThreads);
	const ot = get(openThreads);
	let changed = false;
	const updated = { ...ot };

	for (const [channel, threadIds] of Object.entries(updated)) {
		const remaining = new Set<string>();
		for (const id of threadIds) {
			const thread = tracked[id];
			if (thread && now - new Date(thread.lastActivity).getTime() < TWELVE_HOURS_MS) {
				remaining.add(id);
			} else {
				changed = true;
			}
		}
		if (remaining.size !== threadIds.size) {
			updated[channel] = remaining;
			setOpenThreads(channel, [...remaining]);
		}
	}

	if (changed) {
		openThreads.set(updated);
	}
}, AUTO_CLOSE_INTERVAL_MS);

// Fetch the list of available channels
export async function fetchChannels(includeArchived = false): Promise<Channel[]> {
	try {
		const url = includeArchived ? `${getApiBase()}/channels?include_archived=true` : `${getApiBase()}/channels`;
		const res = await fetch(url);
		if (res.ok) {
			const data = await res.json();
			const channelList: Channel[] = data.channels.map(
				(ch: string | { name: string; is_archived?: boolean; is_dm?: boolean }) => ({
					name: typeof ch === "string" ? ch : ch.name,
					unread: 0,
					has_pr: false,
					ci_status: null,
					is_archived: typeof ch === "object" && ch.is_archived,
					is_dm: typeof ch === "object" ? ch.is_dm || ch.name.startsWith("dm-") : ch.startsWith("dm-"),
				}),
			);
			// Backend already returns channels sorted with main project channel first
			channels.set(channelList);
			// After channels are loaded, fetch open threads for each
			for (const ch of channelList) {
				if (!ch.is_dm && !ch.is_archived) {
					fetchOpenThreads(ch.name).then((threads) => {
						openThreads.update((ot) => ({
							...ot,
							[ch.name]: new Set(threads),
						}));
					});
				}
			}
			return channelList;
		}
	} catch (err) {
		// Retain last-known-good channel list on transient network errors.
		// The channel list will refresh on WebSocket reconnect or next poll.
		console.warn("Failed to fetch channels (retaining cached data):", err);
	}
	return [];
}

// Archive a channel
export async function archiveChannel(channelName: string): Promise<{ ok: boolean; error?: string }> {
	try {
		const res = await fetch(`${getApiBase()}/channels/${encodeURIComponent(channelName)}/archive`, {
			method: "POST",
		});
		if (res.ok) {
			return { ok: true };
		}
		const data = await res.json().catch(() => ({}));
		return { ok: false, error: data.error || `HTTP ${res.status}` };
	} catch (err: unknown) {
		console.error("Failed to archive channel:", err);
		return { ok: false, error: (err as Error).message };
	}
}

// Unarchive a channel
export async function unarchiveChannel(channelName: string): Promise<{ ok: boolean; error?: string }> {
	try {
		const res = await fetch(`${getApiBase()}/channels/${encodeURIComponent(channelName)}/unarchive`, {
			method: "POST",
		});
		if (res.ok) {
			return { ok: true };
		}
		const data = await res.json().catch(() => ({}));
		return { ok: false, error: data.error || `HTTP ${res.status}` };
	} catch (err: unknown) {
		console.error("Failed to unarchive channel:", err);
		return { ok: false, error: (err as Error).message };
	}
}

// Switch to a different project by name
export function switchProject(projectName: string, webhookPort: number | null): void {
	// Disconnect existing WebSocket
	if (ws) {
		ws.close();
		ws = null;
	}
	if (reconnectTimeout) {
		clearTimeout(reconnectTimeout);
		reconnectTimeout = null;
	}
	if (statusPollInterval) {
		clearInterval(statusPollInterval);
		statusPollInterval = null;
	}
	if (usagePollInterval) {
		clearInterval(usagePollInterval);
		usagePollInterval = null;
	}

	// Clear current state
	messages.set([]);
	messagesByChannel.set({ [projectName]: [] });
	channels.set([{ name: projectName, unread: 0, has_pr: false, ci_status: null }]);
	activeChannel.set(projectName);
	coworkers.set([]);
	daemonStatus.set(null);
	kanbanData.set({ backlog: [], inProgress: [], review: [], done: [] });
	repoStatus.set({
		repoName: "",
		fullName: "",
		commitHash: "",
		commitTime: null,
		ciStatus: null,
		releaseTag: null,
		releaseTime: null,
	});
	repoStatuses.set([]);
	usageData.set([]);
	threadForkOwners.set({});
	threadData.set(null);
	threadOwnership.set({});
	// Clear tracked threads when switching to a different project.
	// On same-project reload, the stores (initialized from localStorage)
	// should be preserved.  Use the persisted project name so the first
	// switchProject call after a page reload still detects a project change.
	const previousProject = get(activeProject);
	const savedThreadProject =
		typeof localStorage !== "undefined" ? localStorage.getItem("midtown_thread_project") : null;
	const lastProject = previousProject || savedThreadProject;
	if (lastProject !== projectName) {
		trackedThreads.set({});
		threadUnreadCounts.set({});
	}
	if (typeof localStorage !== "undefined") {
		localStorage.setItem("midtown_thread_project", projectName);
	}
	connected.set(false);

	// Set the new active project
	activeProject.set(projectName);

	if (webhookPort) {
		if (window.location.protocol === "https:") {
			// HTTPS: proxy through the webserver to avoid mixed content errors.
			// The webserver forwards requests to the daemon's webhook port.
			projectApiBase = `${window.location.origin}/api/projects/${projectName}/proxy`;
		} else {
			// HTTP: connect to the project's daemon directly via its webhook port
			projectApiBase = `http://${window.location.hostname}:${webhookPort}`;
		}
	} else {
		// No webhook port - project daemon may not be running
		projectApiBase = "";
	}

	// Load data from the new project
	if (projectApiBase) {
		// Fetch channels first to populate the sidebar immediately.
		// Note: fetchHistory() also builds a channel list from messages,
		// but this ensures all channels (including empty ones) appear immediately.
		fetchChannels();
		fetchHistory();
		fetchStatus();
		fetchUsage();
		connectWebSocket();
		// Poll status every 30s (matching daemon's kanban cache TTL)
		statusPollInterval = setInterval(fetchStatus, 30000);
		// Poll usage every 2 minutes (matching TUI refresh interval)
		usagePollInterval = setInterval(fetchUsage, 120000);
	}
}

// Get the API base for the current project
export function getApiBase(): string {
	return projectApiBase ? `${projectApiBase}/api` : "/api";
}

// Backward-compat fallback for older history payloads:
// if thread replies are included inline, compute reply_count/last_reply on
// parents and filter replies from the main timeline.
function annotateThreadReplyCounts(msgs: Message[]): Message[] {
	const replyCountMap: Record<string, number> = {};
	const lastReplyMap: Record<string, Message> = {};
	const participantsMap: Record<string, string[]> = {};
	for (const m of msgs) {
		if (m.thread_parent_id && !isToolOnly(m)) {
			replyCountMap[m.thread_parent_id] = (replyCountMap[m.thread_parent_id] || 0) + 1;
			lastReplyMap[m.thread_parent_id] = m;
			if (!participantsMap[m.thread_parent_id]) participantsMap[m.thread_parent_id] = [];
			if (!participantsMap[m.thread_parent_id].includes(m.from)) {
				participantsMap[m.thread_parent_id].push(m.from);
			}
		}
	}
	return msgs
		.filter((m) => !m.thread_parent_id)
		.map((m) =>
			replyCountMap[m.id]
				? {
						...m,
						reply_count: replyCountMap[m.id],
						last_reply: lastReplyMap[m.id],
						reply_participants: participantsMap[m.id],
					}
				: m,
		);
}

// Fetch channel message history
// If channelName is provided, fetches only that channel's messages.
// Otherwise, fetches all messages from the main channel.
export async function fetchHistory(channelName: string | null = null): Promise<void> {
	// Abort any in-flight fetch for the same channel to prevent out-of-order
	// responses when the user rapidly switches channels.
	const cacheKey = channelName || BULK_FETCH_KEY;
	const prev = fetchHistoryControllers.get(cacheKey);
	if (prev) prev.abort();
	const controller = new AbortController();
	fetchHistoryControllers.set(cacheKey, controller);

	try {
		const url = channelName
			? `${getApiBase()}/channels/history?channel=${encodeURIComponent(channelName)}`
			: `${getApiBase()}/channels/history`;
		const res = await fetch(url, { signal: controller.signal });
		if (res.ok) {
			const data = await res.json();

			if (channelName) {
				// Fetching a specific channel - update only that channel's messages
				const channelMsgs = annotateThreadReplyCounts(data);

				// Guard: don't replace non-empty channel data with an empty response.
				// The backend can transiently return [] when the channel file is briefly
				// missing (rotation, archiving race). Same "retain last-known-good"
				// pattern used in the catch block below.
				//
				// Check data.length (raw response), not channelMsgs.length: a response
				// containing only thread replies is non-empty but annotateThreadReplyCounts
				// filters them all out, producing channelMsgs=[]. That's real data and
				// must not be treated as a transient empty response.
				if (data.length === 0) {
					const existing = get(messagesByChannel)[channelName];
					if (existing && existing.length > 0) {
						// Strip pending (optimistic) messages from retained data so they
						// don't linger as ghosts (mirroring the bulk-fetch path's cleanup).
						const confirmed = existing.filter((m) => !m.pending);
						if (confirmed.length > 0) {
							console.warn(
								`fetchHistory('${channelName}'): backend returned empty — retaining ${confirmed.length} cached messages`,
							);
							messagesByChannel.update((byChannel) => ({
								...byChannel,
								[channelName]: confirmed,
							}));
							return;
						}
					}
				}

				messagesByChannel.update((byChannel) => ({
					...byChannel,
					[channelName]: channelMsgs,
				}));
			} else {
				// Fetching all messages (initial load) - group by channel
				const byChannel: Record<string, Message[]> = {};
				for (const msg of data) {
					const name = msg.channel || get(activeProject);
					if (!byChannel[name]) {
						byChannel[name] = [];
					}
					byChannel[name].push(msg);
				}

				// Compute reply counts and filter thread replies from main timeline.
				// annotateThreadReplyCounts returns only top-level messages (thread
				// replies filtered out), keeping both stores consistent with the
				// real-time WS handler which never adds thread replies to messages[].
				for (const [ch, channelMsgs] of Object.entries(byChannel)) {
					byChannel[ch] = annotateThreadReplyCounts(channelMsgs);
				}

				// Set legacy store with filtered (top-level only) messages so it
				// stays consistent with messagesByChannel and the WS handler.
				messages.set(Object.values(byChannel).flat());

				// Merge rather than replace: preserve messages for channels not in this
				// response (e.g. channels with no recent history). Using .set() would
				// wipe them on WS reconnect, causing blank channels until re-tapped.
				//
				// Pre-merge: strip any pending (optimistic) messages from existing channels.
				// If the WS echo was lost during a disconnect, a pending message in a
				// low-traffic channel would survive the merge as a "ghost" forever. Clearing
				// pending entries first is safe — if the message actually sent, it comes back
				// clean in byChannel (for its channel) or is simply gone (network loss).
				messagesByChannel.update((existing) => {
					const withoutPending = Object.fromEntries(
						Object.entries(existing).map(([ch, msgs]) => [ch, msgs.filter((m) => !m.pending)]),
					);
					return { ...withoutPending, ...byChannel };
				});

				// Channels are already populated by fetchChannels() which calls the
				// backend's Channel::list(). We no longer derive channels from message
				// content to avoid showing ghost channels for invalid/deleted .jsonl files.
			}
		}
	} catch (err: unknown) {
		if ((err as Error).name === "AbortError") {
			// Request was cancelled by a newer fetchHistory call — expected during
			// rapid channel switching. No action needed.
			return;
		}
		// Retain last-known-good data on transient network errors so the
		// channel view doesn't flash empty. Messages will refresh on the
		// next successful WebSocket reconnect or manual channel switch.
		console.warn("Failed to fetch history (retaining cached data):", err);
	} finally {
		// Clean up controller if it's still the active one for this key
		if (fetchHistoryControllers.get(cacheKey) === controller) {
			fetchHistoryControllers.delete(cacheKey);
		}
	}
}

// Fetch daemon/coworker status and update kanban data
export async function fetchStatus(): Promise<void> {
	try {
		const res = await fetch(`${getApiBase()}/status`);
		if (res.ok) {
			const data = await res.json();
			daemonStatus.set(data);
			coworkers.set(data.coworkers || []);
			if (data.max_in_progress_tasks !== undefined) {
				maxInProgressTasks.set(data.max_in_progress_tasks);
			}
			userSenderName.set(data.user_display_name || "user");
			updateKanbanData(data);
			updateRepoStatus(data);
		}
	} catch (err) {
		console.error("Failed to fetch status:", err);
	}
	// Hydrate pending questions from daemon (survives page refresh / WebSocket reconnect)
	try {
		const res = await fetch(`${getApiBase()}/questions`);
		if (res.ok) {
			const data = await res.json();
			pendingQuestions.set(data.questions || []);
		}
	} catch {
		// Non-critical — questions will arrive via WebSocket events
	}
}

// Fetch API usage data (session + weekly utilization)
export async function fetchUsage(): Promise<void> {
	try {
		const res = await fetch(`${getApiBase()}/usage`);
		if (res.status === 204) {
			// 204 No Content means no credentials available — clear the store
			// so the UI shows the loading/empty state instead of stale data.
			usageData.set([]);
			return;
		}
		if (res.ok) {
			const data = await res.json();
			// Extract usage array from response (backend provides both array and flat fields for backwards compat)
			usageData.set(data.usage || []);
		}
	} catch (err) {
		// Retain last-known-good data on transient network errors so the
		// UsageBars component doesn't disappear and reappear. Data will
		// refresh on the next successful 2-minute poll cycle.
		console.warn("Failed to fetch usage (retaining cached data):", err);
	}
}

function updateKanbanData(data: DaemonStatus): void {
	const tasks = data.tasks || [];
	const prs = data.pull_requests || [];
	const mergedPrs = data.merged_prs || [];

	kanbanData.set({
		backlog: tasks.filter((t) => t.status === "pending"),
		inProgress: tasks.filter((t) => t.status === "in_progress"),
		completedTasks: tasks.filter((t) => t.status === "completed"),
		review: prs.map((pr) => ({
			number: pr.number,
			title: pr.title,
			author: pr.author,
			status: pr.status,
			ci_status: pr.ci_status || "unknown",
			reviewer: pr.reviewer,
			reviewer_assigned_at: pr.reviewer_assigned_at,
			review_posted: pr.review_posted || false,
			created_at: pr.created_at,
			repo: pr.repo || null,
			task_id: pr.task_id,
			task_name: pr.task_name,
		})),
		done: mergedPrs.slice(0, 10).map((pr) => ({
			number: pr.number,
			title: pr.title,
			mergedAt: pr.mergedAt,
			repo: pr.repo || null,
		})),
	});
}

function updateRepoStatus(data: DaemonStatus): void {
	const rs = data.repo_status || {};
	repoStatus.set({
		repoName: data.repo_name || "",
		fullName: data.repo_full_name || "",
		commitHash: rs.commit_hash || "",
		commitTime: rs.commit_time || null,
		ciStatus: rs.ci_status || null,
		releaseTag: rs.release_tag || null,
		releaseTime: rs.release_time || null,
	});

	// Always update multi-repo statuses (empty array clears previous entries on project switch)
	repoStatuses.set(data.repo_statuses || []);
}

// Connect to WebSocket for live updates
export function connectWebSocket(): void {
	if (ws) {
		ws.close();
	}

	const base = projectApiBase || `${window.location.protocol}//${window.location.host}`;
	const protocol = base.startsWith("https") ? "wss:" : "ws:";
	const host = base.replace(/^https?:\/\//, "");
	const wsUrl = `${protocol}//${host}/api/ws`;

	ws = new WebSocket(wsUrl);

	ws.onopen = () => {
		console.log("WebSocket connected");
		connected.set(true);

		// Always fetch history on connect/reconnect to ensure we have all messages.
		// This covers: initial page load, reconnection after network loss,
		// and page becoming active again after being backgrounded.
		const wasReconnect = reconnectTimeout !== null;
		if (reconnectTimeout) {
			clearTimeout(reconnectTimeout);
			reconnectTimeout = null;
		}

		// Fetch history for all channels (main bulk load) to catch up on missed messages.
		fetchHistory();
		// Also fetch the currently active channel specifically, so it refreshes
		// even if it's a topic channel. The bulk fetchHistory() only returns the
		// main channel; topic channels need a targeted fetch.
		const currentChannel = get(activeChannel);
		const projectName = get(activeProject);
		if (currentChannel && currentChannel !== projectName) {
			fetchHistory(currentChannel);
		}
		console.log(wasReconnect ? "Reconnected - fetching message history" : "Connected - loading initial history");
	};

	ws.onclose = () => {
		console.log("WebSocket disconnected");
		connected.set(false);
		// Auto-reconnect after 3 seconds
		reconnectTimeout = setTimeout(connectWebSocket, 3000);
	};

	ws.onerror = (err) => {
		console.error("WebSocket error:", err);
	};

	ws.onmessage = (event) => {
		try {
			const update = JSON.parse(event.data);
			handleUpdate(update);
		} catch (err) {
			console.error("Failed to parse message:", err);
		}
	};
}

// Callbacks for handling error responses from the server
const errorCallbacks = new Map<number, (msg: string) => void>();
let nextErrorCallbackId = 1;

// Register a callback to handle the next error from the server
// Returns a callback ID that can be used to unregister if needed
export function onNextError(callback: (msg: string) => void): number {
	const id = nextErrorCallbackId++;
	errorCallbacks.set(id, callback);
	return id;
}

// Unregister an error callback
export function clearErrorCallback(id: number): void {
	errorCallbacks.delete(id);
}

// Handle incoming WebSocket updates.
// Exported for testing only — production code uses this via the WS onmessage handler.
export function handleUpdate(update: Record<string, unknown>): void {
	switch (update.type) {
		case "channel_message": {
			const msg = update.data as Message;
			const channelName = msg.channel || get(activeProject) || "midtown";

			if (msg.thread_parent_id) {
				const threadParentId = msg.thread_parent_id;
				// Thread reply — update thread panel if open for this parent, and
				// bump reply_count on the parent message.
				threadData.update((td) => {
					if (td && td.parentMessage?.id === threadParentId) {
						// Remove the first pending optimistic reply with matching content/sender before
						// appending the real server-confirmed message. Only match when the confirmed
						// message is from 'user' (guards against a different participant posting the
						// same text and incorrectly consuming our placeholder).
						let threadDeduplicated = false;
						const withoutPending = td.messages.filter((m) => {
							if (!threadDeduplicated && m.pending && m.content === msg.content && msg.from === "user") {
								threadDeduplicated = true;
								return false;
							}
							return true;
						});
						return { ...td, messages: [...withoutPending, msg] };
					}
					return td;
				});

				// Increment reply_count on the parent message in messagesByChannel
				// (skip tool-only messages — they're visual noise, not conversation)
				if (!isToolOnly(msg)) {
					messagesByChannel.update((byChannel) => {
						const channelMsgs = byChannel[channelName];
						if (!channelMsgs) return byChannel;
						return {
							...byChannel,
							[channelName]: channelMsgs.map((m: Message) => {
								if (m.id === threadParentId) {
									const participants = m.reply_participants || [];
									return {
										...m,
										reply_count: (m.reply_count || 0) + 1,
										last_reply: msg,
										reply_participants: participants.includes(msg.from) ? participants : [...participants, msg.from],
									};
								}
								return m;
							}),
						};
					});
				}

				// Thread sidebar tracking: auto-track threads when someone replies to the
				// user's own message, and increment unread for tracked threads.
				// Compare against both 'user' and the configured user_display_name to avoid
				// counting the user's own replies as unread.
				// Skip tool-only messages — they inflate unread badges with visual noise.
				if (!isToolOnly(msg) && msg.from !== "user" && msg.from !== get(userSenderName)) {
					// Auto-track: if the parent message was sent by the user, track
					// the thread in the sidebar so the user sees replies to their messages.
					// Pass reply content so fullText shows the reply, not the parent.
					// replyCount is omitted so trackThread initializes to 0 for new
					// entries (or preserves existing) — the update block below handles the +1.
					const channelMsgs = get(messagesByChannel)[channelName];
					const parentMsg = channelMsgs?.find((m: Message) => m.id === threadParentId);
					const uName = get(userSenderName);
					if (parentMsg && (parentMsg.from === "user" || parentMsg.from === uName)) {
						trackThread(threadParentId, channelName, parentMsg.content, { replyContent: msg.content });
					}

					const tracked = get(trackedThreads);
					const td = get(threadData);
					const panelShowingThis = td && td.parentMessage?.id === threadParentId;
					if (tracked[threadParentId] && !panelShowingThis) {
						threadUnreadCounts.update((counts) => ({
							...counts,
							[threadParentId]: (counts[threadParentId] || 0) + 1,
						}));
					}
					// Update lastActivity/replyCount on the tracked entry
					if (tracked[threadParentId]) {
						const replyFullText = extractPlainText(msg.content);
						trackedThreads.update((t) => ({
							...t,
							[threadParentId]: {
								...t[threadParentId],
								lastActivity: new Date().toISOString(),
								replyCount: (t[threadParentId]?.replyCount || 0) + 1,
								...(replyFullText ? { fullText: replyFullText } : {}),
							},
						}));
					}

					// Reopen thread in sidebar if it was auto-closed (not in openThreads)
					const updatedTracked = get(trackedThreads);
					const threadInfo = updatedTracked[threadParentId];
					if (threadInfo) {
						const ot = get(openThreads);
						const channelThreads = ot[threadInfo.channelName];
						if (!channelThreads || !channelThreads.has(threadParentId)) {
							openThreads.update((current) => ({
								...current,
								[threadInfo.channelName]: new Set([...(current[threadInfo.channelName] || []), threadParentId]),
							}));
							setOpenThreads(threadInfo.channelName, [...(ot[threadInfo.channelName] || []), threadParentId]);
						}
					}
				}

				// DM channels: thread replies stay in the thread panel (consistent
				// with regular channels). The coworker owns the DM like a channel lead.
			} else {
				// Top-level message — add to stores, removing any matching pending optimistic message first.
				// Add to legacy messages array
				messages.update((msgs) => [...msgs, msg]);

				// Add to channel-specific messages, deduplicating pending optimistic entries.
				// If the user sent this message optimistically, a pending placeholder with the
				// same content will be in the list. Remove the first such match before appending
				// the server-confirmed message.
				messagesByChannel.update((byChannel) => {
					const channelMsgs = byChannel[channelName] || [];
					// Only dedup when the confirmed message is from 'user': prevents a different
					// channel participant posting identical text from consuming our placeholder.
					let deduplicated = false;
					const withoutPending = channelMsgs.filter((m: Message) => {
						if (!deduplicated && m.pending && m.content === msg.content && msg.from === "user") {
							deduplicated = true;
							return false;
						}
						return true;
					});
					return { ...byChannel, [channelName]: [...withoutPending, msg] };
				});

				// Update channel list - increment unread if not viewing this channel.
				// Only for top-level messages — thread replies don't appear in the
				// main timeline, so they should not increment the unread badge.
				const currentActiveChannel = get(activeChannel);

				// Only update unread counts for channels that already exist in the list.
				// We no longer auto-add channels from message content to prevent ghost
				// channels. New channels will appear after the next fetchChannels() call
				// (triggered by status polling or manual refresh).
				channels.update((channelList) => {
					const existingChannel = channelList.find((ch) => ch.name === channelName);
					if (existingChannel && channelName !== currentActiveChannel) {
						// Channel exists - increment unread if it's not the active channel
						return channelList.map((ch) => (ch.name === channelName ? { ...ch, unread: ch.unread + 1 } : ch));
					}
					return channelList;
				});
			}
			break;
		}
		case "coworker_status": {
			// Skip channel lead sessions (ch-<channel>) and the lead itself.
			// Channel leads are scoped to a specific topic channel and must not
			// appear in the general coworker status panel.
			const coworkerData = update.data as Coworker;
			const name = coworkerData.name;
			if (name && (name.startsWith("ch-") || name.toLowerCase() === "lead")) {
				break;
			}
			coworkers.update((list) => {
				const idx = list.findIndex((c) => c.name === name);
				if (idx >= 0) {
					list[idx] = { ...list[idx], ...coworkerData };
					return [...list];
				}
				return [...list, coworkerData];
			});
			break;
		}
		case "coworker_question": {
			const questionData = update.data as PendingQuestion;
			pendingQuestions.update((qs) => {
				// Replace existing question from same coworker (only one question per coworker at a time)
				const filtered = qs.filter((q) => q.coworker_name !== questionData.coworker_name);
				return [...filtered, questionData];
			});
			break;
		}
		case "channel_list_changed":
			// Re-fetch full channel list from server to get accurate state
			fetchChannels(get(showArchivedChannels));
			break;
		case "open_threads_changed": {
			const { channel, threads } = update.data as { channel: string; threads: string[] };
			openThreads.update((ot) => ({
				...ot,
				[channel]: new Set(threads),
			}));
			break;
		}
		case "thread_ownership": {
			const { thread_parent_id, has_dedicated_session, owner, parent_lead } = update.data as {
				thread_parent_id: string;
				has_dedicated_session: boolean;
				owner?: string;
				parent_lead?: string;
			};
			threadOwnership.update((map) => ({
				...map,
				[thread_parent_id]: has_dedicated_session,
			}));
			// Populate or clear the fork owner for activity dot coloring.
			// When a fork is created, `owner` carries the fork session name
			// (e.g., "park-discuss-ab12"); when destroyed, clear the entry.
			if (has_dedicated_session && owner) {
				threadForkOwners.update((m) => ({ ...m, [thread_parent_id]: owner }));
			} else {
				threadForkOwners.update((m) => {
					const updated = { ...m };
					delete updated[thread_parent_id];
					return updated;
				});
			}
			// Track the parent channel lead's name for fork display.
			if (has_dedicated_session && parent_lead) {
				threadForkParents.update((m) => ({ ...m, [thread_parent_id]: parent_lead }));
			} else {
				threadForkParents.update((m) => {
					const updated = { ...m };
					delete updated[thread_parent_id];
					return updated;
				});
			}
			break;
		}
		case "error":
			// Invoke all registered error callbacks and then clear them
			errorCallbacks.forEach((callback) => callback((update.data as { message: string }).message));
			errorCallbacks.clear();
			break;
		default:
			console.log("Unknown update type:", update.type);
	}
}

// Send a message to the lead via WebSocket
export function sendMessage(
	content: string,
	channel: string | null = null,
	threadParentId: string | null = null,
): void {
	if (ws && ws.readyState === WebSocket.OPEN) {
		const message: Record<string, string> = {
			type: "send_message",
			content,
		};
		// Include channel if specified (null/undefined means use default)
		if (channel) {
			message.channel = channel;
		}
		if (threadParentId) {
			message.thread_parent_id = threadParentId;
		}
		ws.send(JSON.stringify(message));

		// Optimistically add the message to the store immediately so the user sees
		// their message without waiting for the server round-trip.
		const channelName = channel || "midtown";
		const tempId = `pending-${crypto.randomUUID()}`;
		const optimisticMsg = {
			id: tempId,
			from: "user",
			content,
			channel: channelName,
			timestamp: new Date().toISOString(),
			pending: true,
		};

		if (threadParentId) {
			// Auto-track this thread in the sidebar
			trackThread(threadParentId, channelName, null);
			// Thread reply: add to threadData if the panel is open for this parent
			threadData.update((td) => {
				if (!td) return td;
				return { ...td, messages: [...td.messages, optimisticMsg] };
			});
		} else {
			// Top-level message: add to channel message list
			messagesByChannel.update((byChannel) => {
				const channelMsgs = byChannel[channelName] || [];
				return { ...byChannel, [channelName]: [...channelMsgs, optimisticMsg] };
			});
		}
	} else {
		console.error("WebSocket not connected");
	}
}

// Answer a coworker's pending question
export function sendAnswer(coworkerName: string, answer: string): void {
	if (ws && ws.readyState === WebSocket.OPEN) {
		ws.send(
			JSON.stringify({
				type: "answer_question",
				coworker_name: coworkerName,
				answer,
			}),
		);
		// Optimistically remove from pending questions
		pendingQuestions.update((qs) => qs.filter((q) => q.coworker_name !== coworkerName));
	} else {
		console.error("WebSocket not connected");
	}
}

// Create a dedicated session for a thread (fork).
// If onError is provided, it will be called with the error message on failure.
// Returns the error callback ID (or undefined) so callers can clear it on success.
export function forkThread(
	threadParentId: string,
	channelName: string,
	onError?: (msg: string) => void,
): number | undefined {
	if (ws && ws.readyState === WebSocket.OPEN) {
		const errorId = onError ? onNextError(onError) : undefined;
		ws.send(
			JSON.stringify({
				type: "fork_thread",
				thread_parent_id: threadParentId,
				channel: channelName,
			}),
		);
		return errorId;
	} else if (onError) {
		onError("Not connected");
	}
}

// Return a thread to the channel lead (kill dedicated session).
// If onError is provided, it will be called with the error message on failure.
// Returns the error callback ID (or undefined) so callers can clear it on success.
export function unforkThread(
	threadParentId: string,
	channelName: string,
	onError?: (msg: string) => void,
): number | undefined {
	if (ws && ws.readyState === WebSocket.OPEN) {
		const errorId = onError ? onNextError(onError) : undefined;
		ws.send(
			JSON.stringify({
				type: "unfork_thread",
				thread_parent_id: threadParentId,
				channel: channelName,
			}),
		);
		return errorId;
	} else if (onError) {
		onError("Not connected");
	}
}

// Query whether a thread has a dedicated session
export function queryThreadOwnership(threadParentId: string, channelName: string): void {
	if (ws && ws.readyState === WebSocket.OPEN) {
		ws.send(
			JSON.stringify({
				type: "get_thread_ownership",
				thread_parent_id: threadParentId,
				channel: channelName,
			}),
		);
	}
}

// Cancel the lead session's in-flight request (kill + resume).
// Sends to the active channel's lead — project lead or channel lead.
export function cancelLead(channelName: string): void {
	if (ws && ws.readyState === WebSocket.OPEN) {
		ws.send(
			JSON.stringify({
				type: "cancel_lead",
				channel: channelName,
			}),
		);
	}
}

// Send a raw JSON message over the WebSocket (for view_window / leave_window).
// Returns true if the message was sent, false if the WebSocket was not open.
export function sendWsMessage(msg: Record<string, unknown>): boolean {
	if (ws && ws.readyState === WebSocket.OPEN) {
		ws.send(JSON.stringify(msg));
		return true;
	}
	return false;
}

// Fetch auth profiles from the current project's daemon
// If provider is specified, fetches profiles for that provider only.
// If provider is null/undefined, fetches profiles for the default provider (claude).
export async function fetchAuthProfiles(provider: string | null = null): Promise<AuthProfile[]> {
	try {
		const url = provider
			? `${getApiBase()}/auth/profiles?provider=${encodeURIComponent(provider)}`
			: `${getApiBase()}/auth/profiles`;
		const res = await fetch(url);
		if (res.ok) {
			const data = await res.json();
			// Update the legacy store for backward compat
			if (!provider || provider === "claude") {
				authProfiles.set(data);
			}
			return data;
		}
	} catch (err) {
		console.error("Failed to fetch auth profiles:", err);
	}
	return [];
}

// Fetch profiles for all providers and populate authProfilesByProvider.
// Only includes providers that have at least one profile configured.
export async function fetchAllAuthProfiles(): Promise<Record<string, AuthProfile[]>> {
	const providers = ["claude", "codex", "zai"];
	const byProvider: Record<string, AuthProfile[]> = {};

	for (const provider of providers) {
		const profiles = await fetchAuthProfiles(provider);
		if (profiles.length > 0) {
			byProvider[provider] = profiles;
		}
	}

	authProfilesByProvider.set(byProvider);

	// Update legacy store with claude profiles if available
	if (byProvider.claude) {
		authProfiles.set(byProvider.claude);
	}

	return byProvider;
}

// Start an OAuth login flow for a profile.
// The backend spawns the CLI which opens the default browser for OAuth.
// Returns { ok: true } on success, or { ok: false, error: string } on failure.
export async function startAuthLogin(email: string, provider = "claude"): Promise<{ ok: boolean; error?: string }> {
	try {
		const res = await fetch(`${getApiBase()}/auth/login`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ email, provider }),
		});
		if (res.ok) return { ok: true };
		let errorMsg = `Login failed (${res.status})`;
		try {
			const body = await res.json();
			if (body.error) errorMsg = body.error;
		} catch (_) {
			/* response not JSON */
		}
		return { ok: false, error: errorMsg };
	} catch (err) {
		console.error("Failed to start auth login:", err);
		return { ok: false, error: "Network error" };
	}
}

// Switch to a different auth profile via the daemon RPC.
// Parameters:
//   - profile: Profile name to switch to (e.g., "work", "personal")
//   - provider: Provider name ('claude', 'codex', or 'zai')
// Returns { ok: true } on success, or { ok: false, error: string } on failure.
export async function switchAuthProfile(profile: string, provider: string): Promise<{ ok: boolean; error?: string }> {
	authSwitching.set(true);
	try {
		const res = await fetch(`${getApiBase()}/auth/switch`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ profile, provider }),
		});
		if (res.ok) {
			// Refresh all profiles after switching
			await fetchAllAuthProfiles();
			return { ok: true };
		}
		let errorMsg = `Switch failed (${res.status})`;
		try {
			const body = await res.json();
			if (body.error) errorMsg = body.error;
		} catch (_) {
			/* response not JSON */
		}
		console.error("Auth switch failed:", errorMsg);
		return { ok: false, error: errorMsg };
	} catch (err) {
		console.error("Failed to switch auth profile:", err);
		return { ok: false, error: "Network error" };
	} finally {
		authSwitching.set(false);
	}
}

// Upload a file (image or document) to the daemon.
// Returns { ok: true, path, filename } on success, or { ok: false, error } on failure.
export async function uploadFile(
	file: File,
): Promise<{ ok: boolean; path?: string; filename?: string; error?: string }> {
	try {
		const formData = new FormData();
		formData.append("file", file);

		const res = await fetch(`${getApiBase()}/upload`, {
			method: "POST",
			body: formData,
		});

		if (res.ok) {
			const data = await res.json();
			return { ok: true, path: data.path, filename: data.filename };
		}

		let errorMsg = `Upload failed (${res.status})`;
		try {
			const body = await res.json();
			if (body.error) errorMsg = body.error;
		} catch (_) {
			/* response not JSON */
		}
		console.error("Upload failed:", errorMsg);
		return { ok: false, error: errorMsg };
	} catch (err) {
		console.error("Failed to upload file:", err);
		return { ok: false, error: "Network error" };
	}
}

// Fetch thread history (parent message + replies) for a given parent message
export async function fetchThread(channelName: string, parentMessageId: string): Promise<Message[]> {
	try {
		const params = new URLSearchParams({
			channel: channelName,
			thread_parent_id: parentMessageId,
		});
		const res = await fetch(`${getApiBase()}/channels/history?${params}`);
		if (res.ok) {
			const data = await res.json();
			return data;
		}
	} catch (err) {
		console.warn("Failed to fetch thread:", err);
	}
	return [];
}

// Open a thread panel for the given parent message
// Open a thread panel for the given parent message.
// Pass { pushState: false } during deep-link initialization to avoid
// pushing a history entry that replaceNavState would immediately replace.
export function openThread(parentMessage: Message, channelName: string, { pushState = true } = {}): void {
	// Find any tasks associated with this thread's parent message.
	// Check both thread_id and message_id: for tasks created with --thread-id,
	// thread_id (the conversation root) differs from message_id (the announcement).
	const { inProgress, backlog } = get(kanbanData);
	const allTasks = [...inProgress, ...backlog];
	const tasks = allTasks.filter((t) => t.thread_id === parentMessage.id || t.message_id === parentMessage.id);
	// Clear unread count for this thread and auto-track it in the sidebar
	threadUnreadCounts.update((counts) => {
		const next = { ...counts };
		delete next[parentMessage.id];
		return next;
	});
	trackThread(parentMessage.id, channelName, parentMessage.content, { replyCount: parentMessage.reply_count });

	// Show panel immediately with loading state, then populate with replies
	threadData.set({ parentMessage, channelName, messages: [], tasks });
	// Query thread ownership so the UI knows whether a dedicated session exists
	queryThreadOwnership(parentMessage.id, channelName);
	if (pushState) {
		pushNavState({ channel: channelName, thread: parentMessage.id });
	}
	fetchThread(channelName, parentMessage.id).then((fetched) => {
		// Guard against stale fetch: the user may have opened a different thread
		// (or closed the panel) while this fetch was in flight. Only apply results
		// if the panel still refers to the same parent message. Also merge rather
		// than overwrite so any WS-delivered replies that arrived during the fetch
		// are preserved (append them after the fetched history).
		threadData.update((td) => {
			if (!td || td.parentMessage?.id !== parentMessage.id) return td;
			// The backend includes the parent message in the response — extract it
			// so we can upgrade parentMessage with real data from the API.
			const fetchedParent = fetched.find((m) => m.id === parentMessage.id);
			const replies = fetched.filter((m) => m.id !== parentMessage.id);
			// Deduplicate: WS may have already appended some replies
			const fetchedIds = new Set(replies.map((r) => r.id));
			const wsOnly = td.messages.filter((r) => !fetchedIds.has(r.id));
			return {
				...td,
				parentMessage: fetchedParent ?? td.parentMessage,
				messages: [...replies, ...wsOnly],
			};
		});
	});
}

// Open a thread panel for a task, showing task card(s) above the thread.
// If task.thread_id or task.message_id is present, fetches thread replies.
// If neither is present, shows the task card with no backing thread.
export function openTaskThread(task: Task, channelName: string): void {
	if (!task.thread_id && !task.message_id) {
		// No creation message — show task card only, replies sent as top-level messages
		threadData.set({ parentMessage: null, channelName, messages: [], tasks: [task] });
		pushNavState({ channel: channelName });
		return;
	}

	// Resolve thread parent: prefer thread_id (the conversation thread root) over
	// message_id (the announcement message). Falls back to resolving via the
	// creation message's thread_parent_id for legacy tasks without thread_id.
	const channelMsgs = get(messagesByChannel)[channelName] || [];
	const parentMessageId =
		task.thread_id ?? channelMsgs.find((m) => m.id === task.message_id)?.thread_parent_id ?? task.message_id;
	if (!parentMessageId) return;

	// Find all tasks whose thread roots under the same parent
	// Also include completed tasks from kanbanData.completedTasks
	const { inProgress, backlog, completedTasks } = get(kanbanData);
	const allTasks: Task[] = [...inProgress, ...backlog, ...(completedTasks || [])];
	const tasks = allTasks.filter((t) => {
		if (!t.thread_id && !t.message_id) return false;
		const tParent = t.thread_id ?? channelMsgs.find((m) => m.id === t.message_id)?.thread_parent_id ?? t.message_id;
		return tParent === parentMessageId;
	});
	// Always include the clicked task even if not found above
	if (!tasks.find((t) => t.id === task.id)) tasks.unshift(task);

	// Use the real channel message if available so the MessageRow gets correct
	// timestamp, sender, and content.  Fall back to a synthetic stub only when
	// the message hasn't loaded yet (rare edge case).
	const parentMessage: Message = channelMsgs.find((m) => m.id === parentMessageId) ?? {
		id: parentMessageId,
		from: "lead",
		content: task.subject,
		timestamp: new Date().toISOString(),
	};
	threadData.set({ parentMessage, channelName, messages: [], tasks });
	pushNavState({ channel: channelName, thread: parentMessageId });
	fetchThread(channelName, parentMessageId).then((fetched) => {
		threadData.update((td) => {
			if (!td || td.parentMessage?.id !== parentMessageId) return td;
			// Extract parent from response — replaces synthetic stub with real data
			const fetchedParent = fetched.find((m) => m.id === parentMessageId);
			const replies = fetched.filter((m) => m.id !== parentMessageId);
			const fetchedIds = new Set(replies.map((r) => r.id));
			const wsOnly = td.messages.filter((r) => !fetchedIds.has(r.id));
			return {
				...td,
				parentMessage: fetchedParent ?? td.parentMessage,
				messages: [...replies, ...wsOnly],
			};
		});
	});
}

// ── Browser history helpers ──────────────────────────────────────────────────

// Build a URL path for the given navigation state.
// Always includes `channel` when a thread is present so deep-links work
// even when the channel name matches the project name.
function buildNavUrl(state: { channel?: string; thread?: string; msg?: string }): string {
	const project = get(activeProject);
	if (!project) return "/";
	let url = `/${encodeURIComponent(project)}`;
	const needsChannel = state.channel && (state.channel !== project || state.thread);
	if (needsChannel && state.channel) {
		url += `?channel=${encodeURIComponent(state.channel)}`;
	}
	if (state.thread) {
		url += `${url.includes("?") ? "&" : "?"}thread=${encodeURIComponent(state.thread)}`;
	}
	if (state.msg) {
		url += `${url.includes("?") ? "&" : "?"}msg=${encodeURIComponent(state.msg)}`;
	}
	return url;
}

// Push a new history entry for a user-initiated navigation event.
// No-op when handling a popstate event (prevents circular pushes).
export function pushNavState(state: { channel?: string; thread?: string; msg?: string }): void {
	if (_handlingPopstate) return;
	history.pushState(state, "", buildNavUrl(state));
}

// Replace the current history entry (initial state or URL sync).
export function replaceNavState(state: { channel?: string; thread?: string; msg?: string }): void {
	history.replaceState(state, "", buildNavUrl(state));
}

// Set up the popstate listener for browser back/forward navigation.
// Returns a cleanup function to remove the listener.
export function setupHistoryNavigation(): () => void {
	function handlePopstate(e: PopStateEvent) {
		const state = e.state;
		if (!state) return;

		_handlingPopstate = true;
		try {
			// Channel navigation
			if (state.channel && state.channel !== get(activeChannel)) {
				activeChannel.set(state.channel);
				channels.update((list) => list.map((ch) => (ch.name === state.channel ? { ...ch, unread: 0 } : ch)));
				// Always fetch history on navigation (same rationale as selectChannel).
				fetchHistory(state.channel);
			}

			// Thread navigation
			if (state.thread) {
				if (state.msg) {
					deepLinkMsgId.set(state.msg);
				}
				const channel = state.channel || get(activeChannel) || "";
				const channelMsgs = get(messagesByChannel)[channel] || [];
				const parentMsg = channelMsgs.find((m) => m.id === state.thread);
				if (parentMsg) {
					openThread(parentMsg, channel);
				} else {
					// Message not in loaded messages — use a stub; openThread will fetch the data
					openThread({ id: state.thread, from: "", content: "", timestamp: "" } as Message, channel);
				}
			} else {
				threadData.set(null);
			}
		} finally {
			_handlingPopstate = false;
		}
	}

	window.addEventListener("popstate", handlePopstate);
	return () => window.removeEventListener("popstate", handlePopstate);
}

// Close the thread panel.
// Pass { pushState: false } when the caller will push its own history entry
// (e.g. selectChannel, selectDm) to avoid duplicate entries.
export function closeThread({ pushState = true } = {}): void {
	threadData.set(null);
	if (pushState) {
		pushNavState({ channel: get(activeChannel) || undefined });
	}
}

// Search messages across all channels
export async function searchMessages(query: string, limit = 50): Promise<SearchResponse> {
	try {
		const params = new URLSearchParams({ q: query, limit: String(limit) });
		const res = await fetch(`${getApiBase()}/search?${params}`);
		if (res.ok) {
			return await res.json();
		}
		console.error(`Search API returned ${res.status}: ${res.statusText}`);
		return { results: [], query, total: 0, error: true };
	} catch (err) {
		console.error("Failed to search messages:", err);
		return { results: [], query, total: 0, error: true };
	}
}

// Select (or create-then-select) a DM channel for the given coworker name.
// DM channels are named `dm-<coworkerName>` on the backend.
// If the channel doesn't exist yet, it's created first, then selected.
export async function selectDm(coworkerName: string): Promise<void> {
	const channelName = `dm-${coworkerName}`;

	closeThread({ pushState: false });

	const currentChannels = get(channels);
	const matchingLeadChannel = currentChannels.find(
		(ch) => ch.name === coworkerName && !(ch.is_dm || ch.name.startsWith("dm-")),
	);
	if (matchingLeadChannel) {
		// Root leads already own a real channel. Keep accidental DM navigation
		// pinned to that channel instead of recreating a duplicate dm-* mirror.
		activeChannel.set(coworkerName);
		pushNavState({ channel: coworkerName });
		channels.update((channelList) => channelList.map((ch) => (ch.name === coworkerName ? { ...ch, unread: 0 } : ch)));
		fetchHistory(coworkerName);
		return;
	}
	const exists = currentChannels.some((ch) => ch.name === channelName);

	if (!exists) {
		// Optimistically add the DM channel so the sidebar shows it immediately,
		// regardless of whether the backend create or subsequent fetchChannels succeeds.
		channels.update((list) => [...list, { name: channelName, unread: 0, has_pr: false, ci_status: null, is_dm: true }]);

		try {
			const res = await fetch(`${getApiBase()}/channels/create`, {
				method: "POST",
				headers: { "Content-Type": "application/json" },
				body: JSON.stringify({ name: channelName }),
			});
			if (!res.ok) {
				const errorData = await res.json();
				console.error("Failed to create DM channel:", errorData.error);
			} else {
				// Refresh channel list so the sidebar reflects canonical backend state
				await fetchChannels(get(showArchivedChannels));
			}
		} catch (err) {
			console.error("Failed to create DM channel:", err);
		}
	}

	activeChannel.set(channelName);
	pushNavState({ channel: channelName });

	channels.update((channelList) => channelList.map((ch) => (ch.name === channelName ? { ...ch, unread: 0 } : ch)));

	// Always fetch full history on DM switch (same rationale as selectChannel).
	fetchHistory(channelName);
}

// Fetch AGENTS.md content for a channel (optionally filtered by scope).
// Returns { content, source, error } where error is null on success or a
// descriptive string on failure — distinguishing "no data" from "fetch failed".
export async function fetchChannelAgentsMd(
	channel: string,
	scope: string | null = null,
): Promise<{ content: string; source: string; error: string | null } | null> {
	// Abort any in-flight fetch for the same channel to prevent stale responses
	// when the user rapidly switches channels.
	const prev = fetchAgentsMdControllers.get(channel);
	if (prev) prev.abort();
	const controller = new AbortController();
	fetchAgentsMdControllers.set(channel, controller);

	try {
		let url = `${getApiBase()}/channels/${encodeURIComponent(channel)}/agents-md`;
		if (scope) url += `?scope=${encodeURIComponent(scope)}`;
		const res = await fetch(url, { signal: controller.signal });
		if (res.ok) {
			const data = await res.json();
			return { ...data, error: null };
		}
		console.warn("Failed to fetch AGENTS.md:", res.status);
		return { content: "", source: "none", error: `HTTP ${res.status}` };
	} catch (err: unknown) {
		if ((err as Error).name === "AbortError") {
			return null;
		}
		console.warn("Failed to fetch AGENTS.md:", err);
		return { content: "", source: "none", error: (err as Error).message || "Network error" };
	} finally {
		if (fetchAgentsMdControllers.get(channel) === controller) {
			fetchAgentsMdControllers.delete(channel);
		}
	}
}

// Save AGENTS.md content for a channel (scope: "channel" or "project")
export async function saveChannelAgentsMd(
	channel: string,
	content: string,
	scope = "channel",
): Promise<{ ok: boolean; error?: string }> {
	try {
		const res = await fetch(`${getApiBase()}/channels/${encodeURIComponent(channel)}/agents-md`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ content, scope }),
		});
		if (res.ok || res.status === 204) {
			return { ok: true };
		}
		console.error("Failed to save AGENTS.md:", res.status);
		return { ok: false, error: `HTTP ${res.status}` };
	} catch (err: unknown) {
		console.error("Failed to save AGENTS.md:", err);
		return { ok: false, error: (err as Error).message };
	}
}

// Fetch channel working directory
export async function fetchChannelDirectory(channel: string): Promise<{ directory: string | null }> {
	try {
		const res = await fetch(`${getApiBase()}/channels/${encodeURIComponent(channel)}/directory`);
		if (res.ok) {
			return await res.json();
		}
		console.warn("Failed to fetch channel directory:", res.status);
	} catch (err) {
		console.warn("Failed to fetch channel directory:", err);
	}
	return { directory: null };
}

// Save channel working directory
export async function saveChannelDirectory(
	channel: string,
	directory: string,
): Promise<{ ok: boolean; error?: string }> {
	try {
		const res = await fetch(`${getApiBase()}/channels/${encodeURIComponent(channel)}/directory`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ directory }),
		});
		if (res.ok || res.status === 204) {
			return { ok: true };
		}
		const body = await res.text();
		console.error("Failed to save channel directory:", res.status, body);
		return { ok: false, error: body || `HTTP ${res.status}` };
	} catch (err: unknown) {
		console.error("Failed to save channel directory:", err);
		return { ok: false, error: (err as Error).message };
	}
}

// Channel settings API
export async function putChannelSettings(
	channel: string,
	settings: Record<string, unknown>,
): Promise<{ ok: boolean; error?: string }> {
	try {
		const res = await fetch(`${getApiBase()}/channels/${encodeURIComponent(channel)}/settings`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(settings),
		});
		if (res.ok || res.status === 204) {
			return { ok: true };
		}
		const body = await res.text();
		console.error("Failed to save channel settings:", res.status, body);
		return { ok: false, error: body || `HTTP ${res.status}` };
	} catch (err: unknown) {
		console.error("Failed to save channel settings:", err);
		return { ok: false, error: (err as Error).message };
	}
}

export async function fetchChannelSettings(channel: string): Promise<{ show_full_lead_output?: boolean } | null> {
	try {
		const res = await fetch(`${getApiBase()}/channels/${encodeURIComponent(channel)}/settings`);
		if (res.ok) {
			return await res.json();
		}
		// 404 is fine — no settings saved yet
		if (res.status !== 404) {
			console.warn("Failed to fetch channel settings:", res.status);
		}
	} catch (err) {
		console.warn("Failed to fetch channel settings:", err);
	}
	return null;
}

// Fetch available directories across all project repos
export async function fetchDirectories() {
	try {
		const res = await fetch(`${getApiBase()}/directories`);
		if (res.ok) {
			const data = await res.json();
			return data.directories || [];
		}
		console.warn("Failed to fetch directories:", res.status);
	} catch (err) {
		console.warn("Failed to fetch directories:", err);
	}
	return [];
}
