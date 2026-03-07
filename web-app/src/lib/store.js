import { writable } from "svelte/store";

// ── Generic localStorage helpers ──────────────────────────────────────────────
export function loadFromLocalStorage(key, fallback) {
	if (typeof localStorage !== "undefined") {
		const stored = localStorage.getItem(key);
		if (stored) {
			try {
				return JSON.parse(stored);
			} catch (e) {
				console.warn(`Failed to parse stored ${key}:`, e);
			}
		}
	}
	return fallback;
}

export function saveToLocalStorage(key, value) {
	if (typeof localStorage !== "undefined") {
		localStorage.setItem(key, JSON.stringify(value));
	}
}

// ── Debounced localStorage writes ────────────────────────────────────────────
// Store subscriptions fire on every mutation. During WS message bursts this
// can trigger dozens of synchronous localStorage.setItem() calls per second.
// Debouncing coalesces them into a single write after activity settles.
const DEBOUNCE_DELAY_MS = 500;
const debouncedTimers = new Map();
const pendingValues = new Map();

export function debouncedSaveToLocalStorage(key, value) {
	pendingValues.set(key, value);
	if (debouncedTimers.has(key)) {
		clearTimeout(debouncedTimers.get(key));
	}
	debouncedTimers.set(
		key,
		setTimeout(() => {
			debouncedTimers.delete(key);
			pendingValues.delete(key);
			saveToLocalStorage(key, value);
		}, DEBOUNCE_DELAY_MS),
	);
}

export function flushDebouncedSaves() {
	for (const [_key, timer] of debouncedTimers) {
		clearTimeout(timer);
	}
	debouncedTimers.clear();
	for (const [key, value] of pendingValues) {
		saveToLocalStorage(key, value);
	}
	pendingValues.clear();
}

// Flush any pending debounced writes when the user closes/navigates away from the tab.
if (typeof window !== "undefined") {
	window.addEventListener("beforeunload", flushDebouncedSaves);
}

// Channel messages - now keyed by channel name
// Format: { 'midtown': [...messages], 'auth-refactor': [...messages], ... }
export const messagesByChannel = writable({ midtown: [] });

// Load unread counts from localStorage if available
function loadUnreadCounts() {
	return loadFromLocalStorage("midtown_unread_counts", {});
}

// Save unread counts to localStorage
function saveUnreadCounts(channelList) {
	const counts = {};
	channelList.forEach((ch) => {
		if (ch.unread > 0) {
			counts[ch.name] = ch.unread;
		}
	});
	debouncedSaveToLocalStorage("midtown_unread_counts", counts);
}

// List of available channels with metadata
// Format: [{ name: 'midtown', unread: 0, has_pr: false, ci_status: null }, ...]
const storedUnread = loadUnreadCounts();
export const channels = writable([
	{ name: "midtown", unread: storedUnread.midtown || 0, has_pr: false, ci_status: null },
]);

// Subscribe to channels to persist unread counts
channels.subscribe((channelList) => {
	saveUnreadCounts(channelList);
});

// Currently active/selected channel name
export const activeChannel = writable("midtown");

// Legacy: single message array for backward compatibility during transition
export const messages = writable([]);

// WebSocket connection status
export const connected = writable(false);

// Coworker status
export const coworkers = writable([]);

// Maximum number of coworkers that can be spawned
export const maxCoworkers = writable(8);

// Daemon status
export const daemonStatus = writable(null);

// Kanban board data (derived from status API)
export const kanbanData = writable({
	backlog: [],
	inProgress: [],
	review: [],
	done: [],
});

// Repository status (commit, CI, release) - primary repo
export const repoStatus = writable({
	repoName: "",
	fullName: "",
	commitHash: "",
	commitTime: null,
	ciStatus: null,
	releaseTag: null,
	releaseTime: null,
});

// Multi-repo statuses (array of {label, fullName, commitHash, commitTime, ciStatus, releaseTag, releaseTime})
export const repoStatuses = writable([]);

// Multi-project support
// List of discovered projects: [{name, status, daemon_socket, webhook_port}]
export const projects = writable([]);

// Currently selected project name (null = single-project mode)
export const activeProject = writable(null);

// The sender name the daemon uses for user-authored messages.
// Defaults to 'user'; overridden by the configured user_display_name.
export const userSenderName = writable("user");

// Whether the app is running in multi-project mode (always true — served from shared webserver)
export const multiProjectMode = writable(true);

// Auth profiles: Map of provider -> [{name, is_current, has_credentials}]
// Example: { 'claude': [...], 'codex': [...], 'zai': [...] }
export const authProfilesByProvider = writable({});

// Legacy: single flat array for backward compatibility
export const authProfiles = writable([]);

// Currently selected auth provider ('claude', 'codex', 'zai')
export const selectedAuthProvider = writable("claude");

// Whether an auth switch is in progress
export const authSwitching = writable(false);

// API usage data (session + weekly utilization)
// Format: Array of { provider, profile, session_util, session_resets, week_util, week_resets, account_email }
export const usageData = writable([]);

// Active thread state: { parentMessage: {...}|null, channelName: string, messages: [...], tasks: [] } or null
// tasks: array of task objects to display as cards above the parent message
// parentMessage: null when the thread has no backing channel message (task without message_id)
export const threadData = writable(null);

// Deep-link target message ID: when set, ThreadPanel scrolls to and highlights this message.
// Cleared after the scroll/highlight completes.
export const deepLinkMsgId = writable(null);

// Thread ownership: { [threadParentId]: boolean }
// true = dedicated session (fork active), false/missing = channel lead handles
export const threadOwnership = writable({});

// Thread fork owners: { [threadParentId]: agentName }
// Tracks which agent (coworker/lead) owns each thread's fork session.
// Populated from tool_activity events; used to color-code thread activity dots.
export const threadForkOwners = writable({});

// Thread fork parent leads: { [threadParentId]: parentLeadName }
// Tracks the parent channel lead's name for each fork session. Used to display
// fork messages with the parent lead's name and color instead of "fork-XXXX".
export const threadForkParents = writable({});

// Viewport width tracking for responsive breakpoints
// true when viewport > 1024px (wide desktop layout)
export const isWideScreen = writable(false);

// Whether to show archived channels in the channel list (default: false)
export const showArchivedChannels = writable(false);

// Active tab per channel: { [channelName]: 'messages' | 'prs' | 'notes' }
// Keyed by channel name so switching channels preserves tab position.
export const activeChannelTab = writable({});

// Recent tool call activity keyed by channel name.
// 'midtown' holds the main lead's tool calls; topic channel names hold their channel lead's tool calls.
// Format: { 'midtown': [{ item_id, kind, content, status, timestamp }, ...], 'web': [...], ... }
// Each array holds the most recent items (capped at MAX_TOOL_ITEMS_PER_AGENT) for display.
export const agentToolItems = writable({});

// Thread-scoped tool call activity keyed by thread parent message ID.
// When a forked lead works in a thread, its tool calls are stored here instead of agentToolItems.
// Format: { 'msg-1234': [{ item_id, kind, content, status, timestamp }, ...], ... }
export const threadToolItems = writable({});

// Pending questions from coworkers waiting for user input
// Format: [{ id, coworker_name, question, timestamp }, ...]
export const pendingQuestions = writable([]);

// ── Tracked threads (sidebar display) ─────────────────────────────────────────
// Format: { [threadParentId]: { channelName, subject, lastActivity, replyCount } }
export const trackedThreads = writable(loadFromLocalStorage("midtown_tracked_threads", {}));
trackedThreads.subscribe((v) => debouncedSaveToLocalStorage("midtown_tracked_threads", v));

// Per-thread unread counts: { [threadParentId]: number }
export const threadUnreadCounts = writable(loadFromLocalStorage("midtown_thread_unread", {}));
threadUnreadCounts.subscribe((v) => debouncedSaveToLocalStorage("midtown_thread_unread", v));

// Dismissed threads: user clicked X — prevents re-tracking. Stored as array, used as Set.
const _dismissedArr = loadFromLocalStorage("midtown_dismissed_threads", []);
export const dismissedThreads = writable(new Set(_dismissedArr));
dismissedThreads.subscribe((s) => debouncedSaveToLocalStorage("midtown_dismissed_threads", [...s]));
