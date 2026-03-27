import { writable } from "svelte/store";
import type {
	AuthProfile,
	Channel,
	ChannelSettings,
	ChannelTab,
	Coworker,
	DaemonStatus,
	KanbanData,
	Message,
	MultiRepoStatus,
	PendingQuestion,
	Project,
	RepoStatus,
	ThreadData,
	TrackedThread,
	UsageEntry,
} from "./types.ts";

// ── Generic localStorage helpers ──────────────────────────────────────────────
export function loadFromLocalStorage<T>(key: string, fallback: T): T {
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

export function saveToLocalStorage(key: string, value: unknown): void {
	if (typeof localStorage !== "undefined") {
		localStorage.setItem(key, JSON.stringify(value));
	}
}

// ── Debounced localStorage writes ────────────────────────────────────────────
// Store subscriptions fire on every mutation. During WS message bursts this
// can trigger dozens of synchronous localStorage.setItem() calls per second.
// Debouncing coalesces them into a single write after activity settles.
const DEBOUNCE_DELAY_MS = 500;
const debouncedTimers = new Map<string, ReturnType<typeof setTimeout>>();
const pendingValues = new Map<string, unknown>();

export function debouncedSaveToLocalStorage(key: string, value: unknown): void {
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

export function flushDebouncedSaves(): void {
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
// Format: { 'project-name': [...messages], 'auth-refactor': [...messages], ... }
export const messagesByChannel = writable<Record<string, Message[]>>({});

// Save unread counts to localStorage
function saveUnreadCounts(channelList: Channel[]): void {
	const counts: Record<string, number> = {};
	channelList.forEach((ch) => {
		if (ch.unread > 0) {
			counts[ch.name] = ch.unread;
		}
	});
	debouncedSaveToLocalStorage("midtown_unread_counts", counts);
}

// List of available channels with metadata
export const channels = writable<Channel[]>([]);

// Subscribe to channels to persist unread counts
channels.subscribe((channelList) => {
	saveUnreadCounts(channelList);
});

// Currently active/selected channel name (set by switchProject to the project name)
export const activeChannel = writable<string | null>(null);

// Legacy: single message array for backward compatibility during transition
export const messages = writable<Message[]>([]);

// WebSocket connection status
export const connected = writable<boolean>(false);

// Coworker status
export const coworkers = writable<Coworker[]>([]);

// Maximum number of in-progress tasks
export const maxInProgressTasks = writable<number>(8);

// Daemon status
export const daemonStatus = writable<DaemonStatus | null>(null);

// Kanban board data (derived from status API)
export const kanbanData = writable<KanbanData>({
	backlog: [],
	inProgress: [],
	review: [],
	done: [],
});

// Repository status (commit, CI, release) - primary repo
export const repoStatus = writable<RepoStatus>({
	repoName: "",
	fullName: "",
	commitHash: "",
	commitTime: null,
	ciStatus: null,
	releaseTag: null,
	releaseTime: null,
});

// Multi-repo statuses (array of {label, fullName, commitHash, commitTime, ciStatus, releaseTag, releaseTime})
export const repoStatuses = writable<MultiRepoStatus[]>([]);

// Multi-project support
// List of discovered projects: [{name, status, daemon_socket, webhook_port}]
export const projects = writable<Project[]>([]);

// Currently selected project name (null = single-project mode)
export const activeProject = writable<string | null>(null);

// The sender name the daemon uses for user-authored messages.
// Defaults to 'user'; overridden by the configured user_display_name.
export const userSenderName = writable<string>("user");

// Whether the app is running in multi-project mode (always true — served from shared webserver)
export const multiProjectMode = writable<boolean>(true);

// Auth profiles: Map of provider -> [{name, is_current, has_credentials}]
// Example: { 'claude': [...], 'codex': [...], 'zai': [...] }
export const authProfilesByProvider = writable<Record<string, AuthProfile[]>>({});

// Legacy: single flat array for backward compatibility
export const authProfiles = writable<AuthProfile[]>([]);

// Currently selected auth provider ('claude', 'codex', 'zai')
export const selectedAuthProvider = writable<string>("claude");

// Whether an auth switch is in progress
export const authSwitching = writable<boolean>(false);

// API usage data (session + weekly utilization)
// Format: Array of { provider, profile, session_util, session_resets, week_util, week_resets, account_email }
export const usageData = writable<UsageEntry[]>([]);

// Active thread state: { parentMessage: {...}|null, channelName: string, messages: [...], tasks: [] } or null
// tasks: array of task objects to display as cards above the parent message
// parentMessage: null when the thread has no backing channel message (task without message_id)
export const threadData = writable<ThreadData | null>(null);

// Deep-link target message ID: when set, ThreadPanel scrolls to and highlights this message.
// Cleared after the scroll/highlight completes.
export const deepLinkMsgId = writable<string | null>(null);

// Channel-level target message ID: when set, Channel.svelte scrolls to and highlights this message.
// Set by SearchPalette when selecting a search result. Cleared after scroll/highlight completes.
export const channelTargetMsgId = writable<string | null>(null);

// Thread ownership: { [threadParentId]: boolean }
// true = dedicated session (fork active), false/missing = channel lead handles
export const threadOwnership = writable<Record<string, boolean>>({});

// Thread fork owners: { [threadParentId]: agentName }
// Tracks which agent (coworker/lead) owns each thread's fork session.
// Populated from tool_activity events; used to color-code thread activity dots.
export const threadForkOwners = writable<Record<string, string>>({});

// Thread fork parent leads: { [threadParentId]: parentLeadName }
// Tracks the parent channel lead's name for each fork session. Used to display
// fork messages with the parent lead's name and color instead of "fork-XXXX".
export const threadForkParents = writable<Record<string, string>>({});

// Viewport width tracking for responsive breakpoints
// true when viewport > 1024px (wide desktop layout)
export const isWideScreen = writable<boolean>(false);

// Whether to show archived channels in the channel list (default: false)
export const showArchivedChannels = writable<boolean>(false);

// User-defined channel display order. Stores channel names in the preferred order.
// Channels not in this list appear at the end in their default (server) order.
export const channelOrder = writable<string[]>(loadFromLocalStorage("midtown_channel_order", []));
channelOrder.subscribe((v) => debouncedSaveToLocalStorage("midtown_channel_order", v));

// Active tab per channel: { [channelName]: 'messages' | 'prs' | 'notes' | 'settings' }
// Keyed by channel name so switching channels preserves tab position.
export const activeChannelTab = writable<Record<string, ChannelTab>>({});

// Per-channel settings persisted to localStorage.
// Format: { [channelName]: { inlineToolCalls: boolean } }
// inlineToolCalls: when true, tool calls are shown inline in the message
// stream (like DM threads) instead of grouped in the ThreadActivityDrawer.
export const channelSettings = writable<Record<string, ChannelSettings>>(
	loadFromLocalStorage("midtown_channel_settings", {}),
);
channelSettings.subscribe((v) => debouncedSaveToLocalStorage("midtown_channel_settings", v));

// Pending questions from coworkers waiting for user input
// Format: [{ id, coworker_name, question, timestamp }, ...]
export const pendingQuestions = writable<PendingQuestion[]>([]);

// ── Tracked threads (sidebar display) ─────────────────────────────────────────
// Format: { [threadParentId]: { channelName, subject, lastActivity, replyCount } }
export const trackedThreads = writable<Record<string, TrackedThread>>(
	loadFromLocalStorage("midtown_tracked_threads", {}),
);
trackedThreads.subscribe((v) => debouncedSaveToLocalStorage("midtown_tracked_threads", v));

// ── Progress timestamps (needs-attention: stale task detection) ──────────────
// Tracks when each coworker's progress value last changed.
// Keyed by task_id (string). Value is Unix ms timestamp.
// In-memory only — stale detection starts fresh on page reload.
export const progressTimestamps = writable<Record<string, number>>({});

// ── Read state (server-synced) ──────────────────────────────────────────────
// Per-thread and per-channel read timestamps. Synced from daemon API.
export const threadReadState = writable<Record<string, string>>({});
export const channelReadState = writable<Record<string, string>>({});

// Backwards compatibility — Channel.svelte and ThreadList.svelte still reference this.
// Will be removed when those components are updated to use threadReadState.
export const threadUnreadCounts = writable<Record<string, number>>({});
