// ── Tool data blocks ─────────────────────────────────────────────────────────

export interface ToolBlock {
	tool_name: string;
	call_id?: string;
	input?: Record<string, unknown>;
	output?: string | Record<string, unknown> | null;
	error?: boolean;
}

// ── Messages ─────────────────────────────────────────────────────────────────

export interface Message {
	id: string;
	from: string;
	content: string;
	channel?: string;
	timestamp: string;
	thread_parent_id?: string | null;
	tool_data?: ToolBlock[];
	pending?: boolean;
	reply_count?: number;
	last_reply?: Message;
	reply_participants?: string[];
	msg_type?: string;
	auto_output?: boolean;
}

// ── Channels ─────────────────────────────────────────────────────────────────

export interface Channel {
	name: string;
	unread: number;
	has_pr?: boolean;
	ci_status?: string | null;
	is_archived?: boolean;
	is_dm?: boolean;
}

// ── Coworkers ────────────────────────────────────────────────────────────────

export interface Coworker {
	name: string;
	status?: string;
	phase?: string;
	health?: string;
	task_id?: number | null;
	task_name?: string;
	current_task?: string;
	pr_number?: number | null;
	progress?: number | null;
	channel?: string;
}

// ── Tasks ────────────────────────────────────────────────────────────────────

export interface Task {
	id: number;
	subject: string;
	status: string;
	description?: string;
	owner?: string;
	channel?: string;
	thread_id?: string;
	message_id?: string;
}

// ── Pull requests ────────────────────────────────────────────────────────────

export interface PullRequest {
	number: number;
	title: string;
	author?: string;
	status?: string;
	ci_status?: string;
	reviewer?: string | null;
	reviewer_assigned_at?: string | null;
	review_posted?: boolean;
	created_at?: string;
	repo?: string | null;
	task_id?: number | null;
	task_name?: string | null;
}

export interface MergedPullRequest {
	number: number;
	title: string;
	mergedAt?: string;
	repo?: string | null;
}

// ── Kanban board ─────────────────────────────────────────────────────────────

export interface KanbanData {
	backlog: Task[];
	inProgress: Task[];
	completedTasks?: Task[];
	review: PullRequest[];
	done: MergedPullRequest[];
}

// ── Repository status ────────────────────────────────────────────────────────

export interface RepoStatus {
	repoName: string;
	fullName: string;
	commitHash: string;
	commitTime: string | null;
	ciStatus: string | null;
	releaseTag: string | null;
	releaseTime: string | null;
}

export interface MultiRepoStatus {
	label?: string;
	fullName?: string;
	commitHash?: string;
	commitTime?: string | null;
	ciStatus?: string | null;
	releaseTag?: string | null;
	releaseTime?: string | null;
}

// ── Auth ─────────────────────────────────────────────────────────────────────

export interface AuthProfile {
	name: string;
	is_current: boolean;
	has_credentials: boolean;
}

// ── Usage ────────────────────────────────────────────────────────────────────

export interface UsageEntry {
	provider: string;
	profile: string;
	session_util: number;
	session_resets: number;
	week_util: number;
	week_resets: number;
	account_email: string;
}

// ── Threads ──────────────────────────────────────────────────────────────────

export interface ThreadData {
	parentMessage: Message | null;
	channelName: string;
	messages: Message[];
	tasks: Task[];
}

export interface TrackedThread {
	channelName: string;
	subject: string;
	fullText?: string;
	lastActivity: string;
	replyCount: number;
}

// ── Pending questions ────────────────────────────────────────────────────────

export interface PendingQuestion {
	id: string;
	coworker_name: string;
	question: string;
	timestamp: string;
}

// ── Channel settings ─────────────────────────────────────────────────────────

export interface ChannelSettings {
	inlineToolCalls?: boolean;
}

export type ChannelTab = "messages" | "prs" | "notes" | "settings";

// ── Projects ─────────────────────────────────────────────────────────────────

export interface Project {
	name: string;
	status: string;
	daemon_socket: string;
	webhook_port: number;
}

// ── Search ───────────────────────────────────────────────────────────────────

export interface SearchResult {
	id: string;
	from: string;
	content: string;
	channel: string;
	timestamp: string;
	thread_parent_id?: string | null;
	snippet?: string;
}

export interface SearchResponse {
	results: SearchResult[];
	query: string;
	total: number;
	error?: boolean;
}

// ── Daemon status (raw API response) ─────────────────────────────────────────

export interface DaemonStatus {
	coworkers?: Coworker[];
	max_coworkers?: number;
	user_display_name?: string;
	tasks?: Task[];
	pull_requests?: PullRequest[];
	merged_prs?: MergedPullRequest[];
	repo_name?: string;
	repo_full_name?: string;
	repo_status?: {
		commit_hash?: string;
		commit_time?: string | null;
		ci_status?: string | null;
		release_tag?: string | null;
		release_time?: string | null;
	};
	repo_statuses?: MultiRepoStatus[];
	lead_working?: boolean;
	channel_leads_working?: Record<string, boolean>;
}
