//! Application state and logic for the chat TUI

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use midtown::tasks::extract_task_id_from_pr_title;
use midtown::{Channel, Message};

use ratatui::text::Line;

use super::mermaid::MermaidCache;
use midtown::usage::UsageData;

/// Cached output from draw_chat_messages() to avoid recomputation on input-only redraws.
///
/// The chat message area is the most expensive part of the render pipeline due to
/// mermaid parsing, markdown rendering, and text wrapping. This cache stores the
/// fully-rendered lines so they can be reused when only the input bar changes.
pub struct MessageRenderCache {
    /// Pre-rendered lines for the chat messages area
    pub lines: Vec<Line<'static>>,
    /// Diagram sources from this render pass
    pub diagram_sources: Vec<String>,
    /// Cache key: hash of inputs that affect message rendering
    pub cache_key: u64,
}

impl MessageRenderCache {
    pub fn new(lines: Vec<Line<'static>>, diagram_sources: Vec<String>, cache_key: u64) -> Self {
        Self {
            lines,
            diagram_sources,
            cache_key,
        }
    }
}

/// A pending question from a coworker waiting for user input
#[derive(Debug, Clone)]
pub struct PendingQuestion {
    #[allow(dead_code)] // Available for future use (e.g., dismiss/answer by ID)
    pub id: u64,
    pub coworker_name: String,
    pub question: String,
    #[allow(dead_code)] // Available for future use (e.g., sorting, age display)
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// A single tool activity entry displayed in the lead indicator area.
pub struct ToolActivityEntry {
    /// Full display string like "✓ Read foo.rs", "✗ Run tests", or "› Write bar.rs".
    pub header: String,
    /// The instant when this entry was first observed as completed (✓ or ✗).
    /// None for in-progress entries (›).
    pub completed_at: Option<std::time::Instant>,
}

/// Info about a clipboard image pending delivery to the lead session.
#[derive(Debug, Clone)]
pub struct PendingImageInfo {
    /// Image dimensions (width, height in pixels). (0, 0) if unknown.
    #[allow(dead_code)] // Used in Task 4+ when Ctrl+V handler sets pending_image
    pub dimensions: (u32, u32),
    /// MIME type (e.g., "image/png")
    #[allow(dead_code)] // Used in Task 4+ when Ctrl+V handler sets pending_image
    pub media_type: String,
}

/// Data fetched from background thread for kanban refresh (PR/repo data only).
struct KanbanData {
    prs: Vec<KanbanPr>,
    merged_prs: Vec<MergedPr>,
    /// Repo metadata from daemon RPC (label, full_name)
    repos: Vec<(String, String)>,
}

/// Data fetched from background thread for coworker status refresh.
///
/// Polled via `coworkers.status` RPC at a faster interval than PR data.
/// No GraphQL involved — always reflects current daemon state.
struct CoworkerStatusData {
    /// Active coworkers with their current status
    coworkers: Vec<CoworkerInfo>,
    /// Maximum number of coworkers from daemon config
    max_coworkers: usize,
    /// Whether the headless lead session is actively working
    lead_working: bool,
    /// Recent tool call activity per agent
    tool_activity: HashMap<String, Vec<String>>,
    /// Pending questions from coworkers waiting for user input
    pending_questions: Vec<PendingQuestion>,
    /// Names of active channel leads (e.g. "auth", "tui")
    channel_lead_names: Vec<String>,
}

/// Coworker status information for the TUI board sidebar
#[derive(Debug, Clone)]
pub struct CoworkerInfo {
    /// Coworker name (e.g., "amsterdam")
    pub name: String,
    /// Current task ID being worked on
    pub task_id: Option<u32>,
    /// Workflow phase abbreviation (e.g., "dev", "PR", "test")
    pub phase: Option<String>,
    /// PR number if one is open for this task
    pub pr_number: Option<u64>,
    /// Health status: "green", "yellow", "red"
    pub health: String,
    /// Auth provider (e.g., "claude", "zai")
    pub provider: String,
    /// Profile name for multi-account support
    pub profile: String,
    /// Progress percentage (0-100) if reported
    pub progress: Option<u8>,
    /// Estimated time remaining (e.g., "~3m", "~30s")
    pub time_estimate: Option<String>,
}

/// Info about a repo in a multi-repo project
#[derive(Debug, Clone)]
pub struct RepoInfo {
    /// Short label (directory name, e.g., "midtown")
    pub label: String,
    /// Full GitHub name (e.g., "btucker/midtown")
    pub full_name: String,
}

/// CI status for the repo status line
#[derive(Debug, Clone, PartialEq, Default)]
pub enum CiStatus {
    #[default]
    Unknown,
    Running,
    Passed,
    Failed,
}

/// Repository status data for the status line above kanban
#[derive(Debug, Clone, Default)]
pub struct RepoStatus {
    /// Short commit hash (7 chars)
    pub commit_hash: String,
    /// Time since last commit
    pub commit_time: Option<DateTime<Utc>>,
    /// CI status
    pub ci_status: CiStatus,
    /// Latest release tag (e.g., "v0.1.0")
    pub release_tag: Option<String>,
    /// Time of latest release
    pub release_time: Option<DateTime<Utc>>,
}

/// A task item for the kanban board
#[derive(Debug, Clone)]
pub struct KanbanTask {
    pub id: String,
    pub subject: String,
    pub owner: Option<String>,
    pub status: TaskStatus,
    /// When the task file was last modified (used as proxy for status change time)
    #[allow(dead_code)] // Will be used in future PR detail views
    pub modified_at: Option<DateTime<Utc>>,
    /// Optional channel assignment for routing coworker messages
    pub channel: Option<String>,
    /// Task IDs this task is blocked by
    #[allow(dead_code)]
    pub blocked_by: Vec<String>,
}

/// Task status for kanban columns
#[derive(Debug, Clone, PartialEq)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
}

/// A PR item for the kanban board (open PRs in Review column)
#[derive(Debug, Clone)]
#[allow(dead_code)] // Transitioning to split-panel layout, PRs will be shown differently
pub struct KanbanPr {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub created_at: DateTime<Utc>,
    pub ci_status: CiStatus,
    /// Reviewer name (extracted from review comment frontmatter)
    pub reviewer: Option<String>,
    /// When the reviewer was assigned or the review comment was posted
    pub reviewed_at: Option<DateTime<Utc>>,
    /// Whether the review has been posted (true) vs reviewer is still working (false)
    pub review_posted: bool,
    /// Repository name (for multi-repo projects)
    pub repo: Option<String>,
    /// Task ID extracted from PR title (from `[Midtown !XX]` format)
    pub task_id: Option<u64>,
    /// Task name/subject looked up from task list
    pub task_name: Option<String>,
    /// Whether the PR has merge conflicts
    pub has_conflicts: bool,
}

/// A merged PR item for the Done column
#[derive(Debug, Clone)]
#[allow(dead_code)] // Transitioning to split-panel layout
pub struct MergedPr {
    pub number: u64,
    pub title: String,
    pub merged_at: DateTime<Utc>,
    /// Repository name (for multi-repo projects)
    pub repo: Option<String>,
}

/// Number of messages to load initially and per history load
const INITIAL_MESSAGE_COUNT: usize = 100;

/// Maximum number of messages to keep loaded in memory.
/// When loading history, if we exceed this limit, we stop loading more.
/// This prevents unbounded memory growth when scrolling through large channel logs.
const MAX_LOADED_MESSAGES: usize = 500;

/// How long an optimistic "thinking" state lasts before expiring.
///
/// When a user submits a message to a channel lead, we immediately show a spinner
/// for up to this duration, before real tool activity arrives from the daemon.
pub const CHANNEL_LEAD_THINKING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Which pane has focus in the split-panel layout
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusedPane {
    /// Board panel (left) - channel swimlanes with tasks
    Board,
    /// Chat panel (right) - message stream for selected channel
    Chat,
    /// Input bar (bottom of chat panel) - text input for posting messages
    InputBar,
    /// Thread panel (right side) - thread input and message view
    Thread,
}

/// Identifies a selectable item in the board panel
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoardSelection {
    /// A channel header (channel name)
    Channel(String),
    /// A task within a channel (channel name, task ID)
    Task(String, String),
}

/// Application state
pub struct App {
    /// All messages from the channel (VecDeque for O(1) front insertion)
    pub messages: VecDeque<Message>,
    /// Current scroll offset (0 = most recent at bottom)
    pub scroll_offset: usize,
    /// Visible height for chat panel (updated during render)
    pub visible_height: usize,
    /// Channel for reading messages
    channel: Option<Channel>,
    /// Whether initial messages have been loaded
    initial_load_done: bool,
    /// Byte position where loaded history starts (0 means all history loaded)
    history_start_position: u64,
    /// Whether all history has been loaded
    history_fully_loaded: bool,
    /// Test mode: when true, skip daemon communication to avoid side effects
    test_mode: bool,
    /// Test-only: captures the channel argument from the last post_message call
    #[cfg(test)]
    pub last_posted_channel: Option<String>,
    /// Test-only: tracks whether load_channel_messages was called
    #[cfg(test)]
    pub load_channel_messages_called: bool,
    /// Tasks for the kanban board
    pub tasks: Vec<KanbanTask>,
    /// Open PRs for the kanban board (Review column)
    pub prs: Vec<KanbanPr>,
    /// Merged PRs for the Done column
    pub merged_prs: Vec<MergedPr>,
    /// Active coworkers with their current status
    pub coworkers: Vec<CoworkerInfo>,
    /// Whether the headless lead session is actively working
    pub lead_working: bool,
    /// Recent tool call activity per agent, keyed by lowercase agent name.
    /// Contains human-readable semantic headers (e.g., "$ git status", "read src/lib.rs").
    /// Updated from coworkers.status RPC (live, not cached). Cleared when agent posts a message.
    pub tool_activity: HashMap<String, Vec<ToolActivityEntry>>,
    /// Optimistic thinking state: channels where user just submitted a message.
    /// Set immediately on message submit; cleared when real tool activity arrives
    /// or after 30 seconds. Used to show spinner before channel lead responds.
    pub channel_lead_thinking: HashMap<String, std::time::Instant>,
    /// Maximum number of coworkers allowed
    pub max_coworkers: usize,
    /// Pending questions from coworkers waiting for user input
    pub pending_questions: Vec<PendingQuestion>,
    /// Repository name with owner (e.g., "btucker/midtown")
    /// Used for constructing GitHub PR URLs in kanban hyperlinks
    pub repo_name: String,
    /// Last time kanban data was refreshed
    kanban_last_refresh: Instant,
    /// Receiver for async kanban data from background thread
    kanban_receiver: Option<Receiver<KanbanData>>,
    /// Last time coworker status was refreshed
    coworker_status_last_refresh: Instant,
    /// Receiver for async coworker status from background thread
    coworker_status_receiver: Option<Receiver<CoworkerStatusData>>,
    /// Repository status (commit, CI, release info) - primary repo
    pub repo_status: RepoStatus,
    /// Multi-repo statuses (label, full_name, status) for all project repos
    pub repo_statuses: Vec<(RepoInfo, RepoStatus)>,
    /// Last time repo status was refreshed
    repo_status_last_refresh: Instant,
    /// Receiver for async repo status from background thread
    repo_status_receiver: Option<Receiver<Vec<(RepoInfo, RepoStatus)>>>,
    /// User display name from config (None = "user")
    pub user_display_name: Option<String>,
    /// Cached mapping of coworker name -> current task subject.
    /// Rebuilt only when tasks change, not every frame.
    current_tasks_cache: HashMap<String, String>,
    /// Hash of task state used to detect when cache needs rebuilding
    tasks_cache_hash: u64,
    /// Whether user intentionally scrolled to view oldest messages (Home/g key).
    /// This flag distinguishes intentional "view top of history" from scroll_offset
    /// exceeding max_scroll due to visible_height changes (e.g., kanban resizing).
    /// When true AND at max_scroll, line truncation shows oldest content.
    /// When false, always use normal truncation (LAST N lines) for smooth scrolling.
    intentionally_at_top: bool,
    /// Accumulator for mouse wheel scroll events (0-7).
    /// Mouse wheels send multiple events per physical scroll, so we accumulate
    /// fractional scrolls: 8 events = 1 line of movement for smoother scrolling.
    mouse_scroll_accumulator: u8,
    /// Cache for rendered mermaid diagrams (content hash -> PNG image)
    pub mermaid_cache: MermaidCache,
    /// Mermaid diagram sources visible in the current render pass (indexed from 1 in the UI).
    /// Populated during each draw call; used to look up which diagram to open fullscreen.
    pub diagram_sources: Vec<String>,
    /// Current usage data (session + weekly utilization) for all active accounts
    pub usage_data: Vec<UsageData>,
    /// Receiver for async usage data from background thread
    usage_receiver: Option<Receiver<Vec<UsageData>>>,
    /// Last time usage data was refreshed
    usage_last_refresh: Instant,
    /// Which pane currently has focus
    pub focused_pane: FocusedPane,
    /// Currently selected item in the board panel (channel or task)
    pub board_selection: Option<BoardSelection>,
    /// Currently selected channel for viewing messages
    pub selected_channel: String,
    /// Whether the currently selected channel is archived
    pub selected_channel_archived: bool,
    /// Text input buffer for the input bar
    pub input_text: String,
    /// Cursor position in the input text
    pub input_cursor: usize,
    /// Clipboard image pending delivery to the lead on Enter
    #[allow(dead_code)] // Used in Task 4+ Ctrl+V handler and Task 5 Enter handler
    pub pending_image: Option<PendingImageInfo>,
    /// Whether selection mode is active (mouse capture disabled for text selection)
    pub selection_mode: bool,
    /// Cached rendered message lines and hyperlinks to skip recomputation on input-only redraws
    pub message_render_cache: Option<MessageRenderCache>,
    /// Unread message counts per channel (channel_name -> unread_count)
    pub channel_unread_counts: HashMap<String, usize>,
    /// Autocomplete state
    pub autocomplete: AutocompleteState,
    /// Channel switcher overlay state
    pub channel_switcher: ChannelSwitcherState,
    /// Whether to show archived channels in the board panel
    pub show_archived_channels: bool,
    /// Spinner animation frame counter for coworker progress display
    spinner_frame: usize,
    /// Last time the spinner frame was advanced (for time-based animation)
    spinner_last_tick: Instant,
    /// Names of active channel leads (e.g. "auth", "tui"), populated from coworkers.status.
    /// Used to color their messages LightYellow like the main lead.
    pub channel_lead_names: Vec<String>,
    /// List of all available channels (including empty ones)
    pub available_channels: Vec<midtown::ChannelInfo>,
    /// Last time available channels were refreshed
    channels_last_refresh: Instant,
    /// Project/repository name (used for pinning main channel first in sidebar)
    project_name: String,
    /// Last rendered board panel area (for click detection)
    pub board_area: Option<ratatui::layout::Rect>,
    /// Last rendered chat messages area (for click detection)
    pub chat_messages_area: Option<ratatui::layout::Rect>,
    /// Last rendered input bar area (for click detection)
    pub input_area: Option<ratatui::layout::Rect>,
    /// Last rendered thread input area (for click detection; None when thread is closed)
    pub thread_input_area: Option<ratatui::layout::Rect>,
    /// Mapping of board panel line numbers to tasks (for click-to-attach)
    /// Maps (line_number) -> (task_id, task_owner) where line_number is relative to board content area
    pub task_line_map: HashMap<u16, (String, Option<String>)>,
    /// Mapping of board panel line numbers to channel headers (for click-to-select)
    /// Maps line_number -> channel_name where line_number is relative to board content area
    pub channel_line_map: HashMap<u16, String>,
    /// Mapping of chat message area line numbers to thread parent IDs.
    /// Used for click-to-open on "↳ N replies" indicator lines.
    /// Line numbers are relative to the chat content area (inside borders).
    pub thread_reply_line_map: HashMap<u16, String>,
    /// Sidebar width as a percentage of the total horizontal area (20–60).
    /// The rendered width is further capped at `MAX_SIDEBAR_WIDTH` columns.
    pub sidebar_width_pct: u16,
    /// X column of the divider between sidebar and chat (set each render pass)
    pub divider_x: Option<u16>,
    /// Whether the user is currently dragging the divider
    pub dragging_divider: bool,
    /// Full width of the horizontal layout area (set each render pass, used for drag resize)
    pub layout_width: u16,
    /// Y range of the main content area (set each render pass, used for divider Y bounds check)
    pub main_area_y: u16,
    /// Bottom Y (exclusive) of the main content area
    pub main_area_bottom: u16,
    /// Kill ring: stores killed (cut) text for emacs-style Ctrl+Y yank
    pub kill_ring: Option<String>,
    /// Whether the previous command was a kill — consecutive kills append to the kill ring
    pub last_was_kill: bool,
    /// Currently open thread parent message ID
    pub thread_parent_id: Option<String>,
    /// Thread reply messages (messages with thread_parent_id matching the open thread)
    pub thread_messages: Vec<midtown::Message>,
    /// Thread input text (separate from main input)
    pub thread_input_text: String,
    /// Thread input cursor position
    pub thread_input_cursor: usize,
    /// Recent messages from the ops channel (daemon operational messages).
    /// Loaded from the "ops" channel file independently of the selected channel.
    pub ops_messages: VecDeque<Message>,
    /// Channel handle for the ops channel (used for cursor-based polling)
    ops_channel: Option<Channel>,
    /// Whether ops channel initial load has been done
    ops_initial_load_done: bool,
}

/// Autocomplete state for @mentions, #channels, and !task-ids
#[derive(Debug, Clone, Default)]
pub struct AutocompleteState {
    /// Whether autocomplete dropdown is shown
    pub show: bool,
    /// Type of autocomplete trigger: '@' for mentions, '#' for channels, '!' for tasks
    pub trigger_type: Option<char>,
    /// Query string after the trigger character
    pub query: String,
    /// Filtered autocomplete items to display
    pub items: Vec<AutocompleteItem>,
    /// Selected index in the items list
    pub selected_index: usize,
    /// Byte position in input_text where the trigger character starts
    pub trigger_start_pos: usize,
}

/// An autocomplete suggestion item
#[derive(Debug, Clone)]
pub struct AutocompleteItem {
    /// The full value to insert (e.g., "@park", "#auth-refactor", "!42")
    pub value: String,
    /// Optional description (e.g., coworker's current task, channel purpose, task subject)
    pub description: Option<String>,
}

/// Channel switcher overlay state (Ctrl+K quick switcher)
#[derive(Debug, Clone, Default)]
pub struct ChannelSwitcherState {
    /// Whether the channel switcher overlay is shown
    pub show: bool,
    /// Input text for filtering channels
    pub input: String,
    /// Filtered channel list to display
    pub filtered_channels: Vec<ChannelSwitcherItem>,
    /// Selected index in the filtered list
    pub selected_index: usize,
}

/// A channel item in the channel switcher
#[derive(Debug, Clone)]
pub struct ChannelSwitcherItem {
    /// Channel name
    pub name: String,
    /// Unread count for this channel (0 if none)
    pub unread_count: usize,
}

/// Interval between PR/kanban data refreshes (30 seconds).
///
/// PR data requires a GitHub GraphQL round-trip, cached for 60s on the daemon side.
/// 30s is short enough to stay within cache TTL while avoiding redundant fetches.
const KANBAN_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Interval between coworker status refreshes (2 seconds — live in-memory state, no GraphQL)
const COWORKER_STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(2);

/// Interval between repo status refreshes (60 seconds)
const REPO_STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// Interval between usage data refreshes (120 seconds)
const USAGE_REFRESH_INTERVAL: Duration = Duration::from_secs(120);

/// Shorter retry interval when usage fetch fails (15 seconds)
const USAGE_RETRY_INTERVAL: Duration = Duration::from_secs(15);

/// Interval between available channels list refreshes (30 seconds)
const CHANNELS_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Number of lines to scroll per mouse wheel event
const SCROLL_STEP: usize = 3;

impl App {
    pub fn new() -> Self {
        // Use detect_repo_name() which correctly handles worktrees by using
        // git-common-dir, ensuring we read from the same channel as the daemon
        let channel_repo =
            midtown::paths::detect_repo_name().unwrap_or_else(|| "default".to_string());

        let channel = Channel::for_repo(&channel_repo).ok();

        // Open the ops channel for the board sidebar (daemon operational messages)
        let base_dir = midtown::paths::projects_dir_for_repo(&channel_repo);
        let ops_channel = midtown::Channel::new(&base_dir, "ops").ok();

        // Get repo name with owner from gh CLI (e.g., "btucker/midtown")
        let repo_name = fetch_repo_name();

        let mut app = Self {
            messages: VecDeque::new(),
            scroll_offset: 0,
            visible_height: 20,
            channel,
            initial_load_done: false,
            history_start_position: 0,
            history_fully_loaded: false,
            test_mode: false,
            #[cfg(test)]
            last_posted_channel: None,
            #[cfg(test)]
            load_channel_messages_called: false,
            tasks: Vec::new(),
            prs: Vec::new(),
            merged_prs: Vec::new(),
            coworkers: Vec::new(),
            lead_working: false,
            tool_activity: HashMap::new(),
            channel_lead_thinking: HashMap::new(),
            max_coworkers: 10, // Default, will be updated from daemon
            pending_questions: Vec::new(),
            repo_name,
            kanban_last_refresh: Instant::now() - KANBAN_REFRESH_INTERVAL, // Force initial refresh
            kanban_receiver: None,
            coworker_status_last_refresh: Instant::now() - COWORKER_STATUS_REFRESH_INTERVAL, // Force initial refresh
            coworker_status_receiver: None,
            repo_status: RepoStatus::default(),
            repo_statuses: Vec::new(),
            repo_status_last_refresh: Instant::now() - REPO_STATUS_REFRESH_INTERVAL, // Force initial refresh
            repo_status_receiver: None,
            user_display_name: midtown::config::get_user_display_name(),
            current_tasks_cache: HashMap::new(),
            tasks_cache_hash: 0,
            intentionally_at_top: false,
            mouse_scroll_accumulator: 0,
            mermaid_cache: MermaidCache::new(),
            diagram_sources: Vec::new(),
            usage_data: Vec::new(),
            usage_receiver: None,
            usage_last_refresh: Instant::now() - USAGE_REFRESH_INTERVAL, // Force initial refresh
            focused_pane: FocusedPane::Board,
            board_selection: None,
            selected_channel: "midtown".to_string(),
            selected_channel_archived: false,
            input_text: String::new(),
            input_cursor: 0,
            pending_image: None,
            selection_mode: false,
            message_render_cache: None,
            channel_unread_counts: HashMap::new(),
            autocomplete: AutocompleteState::default(),
            channel_switcher: ChannelSwitcherState::default(),
            show_archived_channels: false,
            spinner_frame: 0,
            spinner_last_tick: Instant::now(),
            channel_lead_names: Vec::new(),
            available_channels: Vec::new(),
            channels_last_refresh: Instant::now() - CHANNELS_REFRESH_INTERVAL, // Force initial refresh
            project_name: channel_repo.clone(),
            board_area: None,
            chat_messages_area: None,
            input_area: None,
            thread_input_area: None,
            task_line_map: HashMap::new(),
            channel_line_map: HashMap::new(),
            thread_reply_line_map: HashMap::new(),
            sidebar_width_pct: 40,
            divider_x: None,
            dragging_divider: false,
            layout_width: 0,
            main_area_y: 0,
            main_area_bottom: u16::MAX,
            kill_ring: None,
            last_was_kill: false,
            thread_parent_id: None,
            thread_messages: Vec::new(),
            thread_input_text: String::new(),
            thread_input_cursor: 0,
            ops_messages: VecDeque::new(),
            ops_channel,
            ops_initial_load_done: false,
        };

        // Initial load
        app.refresh();
        // Ensure we start at the bottom (most recent messages)
        app.scroll_to_bottom();
        app
    }

    /// Refresh messages from the channel and kanban data
    pub fn refresh(&mut self) {
        // Use cursor-based reading for efficient tailing
        // The cursor tracks byte position in the file, so we only read new content
        if let Some(ref channel) = self.channel {
            let is_initial_load = !self.initial_load_done;
            if is_initial_load {
                // First load: only load the last N messages for fast startup
                // This avoids reading the entire history on large channels
                if let Ok((messages, start_pos)) =
                    channel.read_last_n_messages(INITIAL_MESSAGE_COUNT)
                {
                    self.messages = VecDeque::from(messages);
                    self.history_start_position = start_pos;
                    self.history_fully_loaded = start_pos == 0;
                    self.scroll_offset = 0; // Start at bottom (most recent)
                }
                // Position cursor at EOF so read_since_cursor only gets NEW messages
                let _ = channel.set_cursor_to_end("chat-tui");
                self.initial_load_done = true;
                return;
            }

            // Read new messages since cursor position
            // On subsequent calls, cursor tracks new messages arriving
            if let Ok(new_messages) = channel.read_since_cursor("chat-tui")
                && !new_messages.is_empty()
            {
                let added = new_messages.len();
                let was_at_bottom = self.scroll_offset == 0;

                // Route thread replies to thread_messages if a thread is open
                if let Some(ref open_thread_id) = self.thread_parent_id {
                    for msg in &new_messages {
                        if msg.thread_parent_id.as_deref() == Some(open_thread_id) {
                            self.thread_messages.push(msg.clone());
                        }
                    }
                }

                // Append new messages (they're already in chronological order)
                self.messages.extend(new_messages);

                if was_at_bottom {
                    // User was at bottom - stay at bottom (auto-scroll)
                    self.scroll_offset = 0;
                } else {
                    // User had scrolled up - adjust offset to stay viewing same messages
                    self.scroll_offset += added;
                }
            }
        }

        // Check for kanban data from background thread (non-blocking)
        if let Some(ref receiver) = self.kanban_receiver {
            match receiver.try_recv() {
                Ok(data) => {
                    self.prs = data.prs;
                    self.merged_prs = data.merged_prs;
                    // Update repo info from daemon if available
                    if !data.repos.is_empty() {
                        let new_repos: Vec<RepoInfo> = data
                            .repos
                            .iter()
                            .map(|(label, full_name)| RepoInfo {
                                label: label.clone(),
                                full_name: full_name.clone(),
                            })
                            .collect();
                        // If repo list changed, update and force status refresh
                        let changed = self.repo_statuses.len() != new_repos.len()
                            || self
                                .repo_statuses
                                .iter()
                                .zip(new_repos.iter())
                                .any(|((info, _), new)| info.full_name != new.full_name);
                        if changed {
                            self.repo_statuses = new_repos
                                .into_iter()
                                .map(|info| (info, RepoStatus::default()))
                                .collect();
                            self.repo_status_last_refresh =
                                Instant::now() - REPO_STATUS_REFRESH_INTERVAL;
                        }
                    }
                    self.kanban_receiver = None; // Clear receiver, fetch complete
                }
                Err(TryRecvError::Empty) => {
                    // Still waiting for data, continue
                }
                Err(TryRecvError::Disconnected) => {
                    // Thread finished without sending (error case), clear receiver
                    self.kanban_receiver = None;
                }
            }
        }

        // Refresh kanban data less frequently - spawn background thread if not already running
        if self.kanban_last_refresh.elapsed() >= KANBAN_REFRESH_INTERVAL
            && self.kanban_receiver.is_none()
        {
            self.refresh_kanban();
            self.kanban_last_refresh = Instant::now();
        }

        // Check for coworker status data from background thread (non-blocking)
        if let Some(ref receiver) = self.coworker_status_receiver {
            match receiver.try_recv() {
                Ok(data) => {
                    self.coworkers = data.coworkers;
                    self.lead_working = data.lead_working;
                    self.tool_activity = merge_tool_activity(
                        std::mem::take(&mut self.tool_activity),
                        data.tool_activity,
                    );
                    self.clear_channel_lead_thinking_for_in_progress();
                    self.max_coworkers = data.max_coworkers;
                    self.pending_questions = data.pending_questions;
                    self.channel_lead_names = data.channel_lead_names;
                    self.coworker_status_receiver = None;
                }
                Err(TryRecvError::Empty) => {
                    // Still waiting for data, continue
                }
                Err(TryRecvError::Disconnected) => {
                    self.coworker_status_receiver = None;
                }
            }
        }

        // Refresh coworker status frequently — cheap RPC call, no GraphQL
        if self.coworker_status_last_refresh.elapsed() >= COWORKER_STATUS_REFRESH_INTERVAL
            && self.coworker_status_receiver.is_none()
        {
            self.refresh_coworker_status();
            self.coworker_status_last_refresh = Instant::now();
        }

        // Check for repo status data from background thread (non-blocking)
        if let Some(ref receiver) = self.repo_status_receiver {
            match receiver.try_recv() {
                Ok(statuses) => {
                    // Update multi-repo statuses
                    self.repo_statuses = statuses;
                    // Keep primary repo_status in sync (first repo)
                    if let Some((_, status)) = self.repo_statuses.first() {
                        self.repo_status = status.clone();
                    }
                    self.repo_status_receiver = None;
                }
                Err(TryRecvError::Empty) => {
                    // Still waiting for data
                }
                Err(TryRecvError::Disconnected) => {
                    self.repo_status_receiver = None;
                }
            }
        }

        // Refresh repo status less frequently
        if self.repo_status_last_refresh.elapsed() >= REPO_STATUS_REFRESH_INTERVAL
            && self.repo_status_receiver.is_none()
        {
            self.refresh_repo_status();
            self.repo_status_last_refresh = Instant::now();
        }

        // Check for usage data from background thread (non-blocking)
        if let Some(ref receiver) = self.usage_receiver {
            match receiver.try_recv() {
                Ok(data) => {
                    if !data.is_empty() {
                        self.usage_data = data;
                        // Only advance refresh timer on success
                        self.usage_last_refresh = Instant::now();
                    } else {
                        // On failure, use shorter retry interval
                        self.usage_last_refresh =
                            Instant::now() - USAGE_REFRESH_INTERVAL + USAGE_RETRY_INTERVAL;
                    }
                    self.usage_receiver = None;
                }
                Err(TryRecvError::Empty) => {
                    // Still waiting for data
                }
                Err(TryRecvError::Disconnected) => {
                    self.usage_receiver = None;
                }
            }
        }

        // Refresh usage data periodically
        if self.usage_last_refresh.elapsed() >= USAGE_REFRESH_INTERVAL
            && self.usage_receiver.is_none()
        {
            let (tx, rx) = mpsc::channel();
            self.usage_receiver = Some(rx);

            // Collect active provider/profile combinations from coworker data
            let active_profiles: Vec<(midtown::auth::AuthProvider, String)> = self
                .coworkers
                .iter()
                .map(|cw| {
                    let provider = cw
                        .provider
                        .parse::<midtown::auth::AuthProvider>()
                        .unwrap_or(midtown::auth::AuthProvider::Claude);
                    (provider, cw.profile.clone())
                })
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();

            // Fall back to current profile if no coworkers
            let profiles_to_fetch = if active_profiles.is_empty() {
                vec![(
                    midtown::auth::AuthProvider::Claude,
                    midtown::auth::current_profile(),
                )]
            } else {
                active_profiles
            };

            thread::spawn(move || {
                let result = midtown::usage::fetch_multi_usage(&profiles_to_fetch);
                let _ = tx.send(result);
            });
        }

        // Poll for completed mermaid renders
        self.mermaid_cache.poll_completed();

        // Refresh available channels list periodically (less frequent than every tick)
        if self.channels_last_refresh.elapsed() >= CHANNELS_REFRESH_INTERVAL {
            self.refresh_available_channels();
            self.channels_last_refresh = Instant::now();
        }

        // Refresh unread counts for channels
        self.refresh_unread_counts();

        // Refresh ops channel messages for the board sidebar
        self.refresh_ops_messages();
    }

    /// Refresh messages from the ops channel for the board sidebar mini-channel.
    ///
    /// The ops channel contains daemon operational messages (spawns, shutdowns,
    /// worktree cleanups, stuck detection, etc.) that were previously filtered
    /// from the main channel using is_ops_message(). Now they route directly to
    /// ops.jsonl, so the sidebar reads from that file independently.
    fn refresh_ops_messages(&mut self) {
        const OPS_MAX: usize = 20;
        if let Some(ref channel) = self.ops_channel {
            if !self.ops_initial_load_done {
                if let Ok((messages, _)) = channel.read_last_n_messages(OPS_MAX) {
                    self.ops_messages = VecDeque::from(messages);
                }
                let _ = channel.set_cursor_to_end("chat-tui-ops");
                self.ops_initial_load_done = true;
                return;
            }
            if let Ok(new_msgs) = channel.read_since_cursor("chat-tui-ops")
                && !new_msgs.is_empty()
            {
                self.ops_messages.extend(new_msgs);
                // Trim to keep only recent messages
                while self.ops_messages.len() > OPS_MAX {
                    self.ops_messages.pop_front();
                }
            }
        }
    }

    /// Refresh kanban board data (tasks and PRs via kanban.data RPC).
    ///
    /// Coworker status is refreshed separately by `refresh_coworker_status`.
    fn refresh_kanban(&mut self) {
        // Tasks are local file reads - fast, can stay synchronous
        self.tasks = fetch_tasks();

        // PRs: try daemon RPC first, fall back to direct gh CLI
        let (tx, rx) = mpsc::channel();
        self.kanban_receiver = Some(rx);

        thread::spawn(move || {
            let (prs, merged_prs, repos) = fetch_kanban_data_via_rpc()
                .unwrap_or_else(|| (fetch_prs(), fetch_merged_prs(), Vec::new()));
            // Ignore send error if receiver dropped (app closed)
            let _ = tx.send(KanbanData {
                prs,
                merged_prs,
                repos,
            });
        });
    }

    /// Refresh coworker status via the `coworkers.status` RPC.
    ///
    /// Runs in a background thread to avoid blocking the TUI. The result is
    /// received in `refresh()` via `coworker_status_receiver`.
    fn refresh_coworker_status(&mut self) {
        let (tx, rx) = mpsc::channel();
        self.coworker_status_receiver = Some(rx);

        thread::spawn(move || {
            let data = fetch_coworker_status_via_rpc().unwrap_or_else(|| CoworkerStatusData {
                coworkers: Vec::new(),
                max_coworkers: 10,
                lead_working: false,
                tool_activity: HashMap::new(),
                pending_questions: Vec::new(),
                channel_lead_names: Vec::new(),
            });
            let _ = tx.send(data);
        });
    }

    /// Refresh repository status (commit, CI, release) for all repos
    fn refresh_repo_status(&mut self) {
        let (tx, rx) = mpsc::channel();
        self.repo_status_receiver = Some(rx);

        // Clone repo info for the background thread
        let repos: Vec<RepoInfo> = self
            .repo_statuses
            .iter()
            .map(|(info, _)| info.clone())
            .collect();

        thread::spawn(move || {
            if repos.is_empty() {
                // Single-repo mode: use current git context
                let status = fetch_repo_status(None);
                let info = RepoInfo {
                    label: String::new(),
                    full_name: String::new(),
                };
                let _ = tx.send(vec![(info, status)]);
            } else {
                // Multi-repo mode: fetch status for each repo
                let statuses: Vec<(RepoInfo, RepoStatus)> = repos
                    .into_iter()
                    .map(|info| {
                        let full_name = if info.full_name.is_empty() {
                            None
                        } else {
                            Some(info.full_name.clone())
                        };
                        let status = fetch_repo_status(full_name.as_deref());
                        (info, status)
                    })
                    .collect();
                let _ = tx.send(statuses);
            }
        });
    }

    /// Get tasks grouped by status for the kanban board
    pub fn tasks_by_status(&self) -> (Vec<&KanbanTask>, Vec<&KanbanTask>, Vec<&KanbanTask>) {
        let pending: Vec<_> = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Pending)
            .collect();
        let in_progress: Vec<_> = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::InProgress)
            .collect();
        let completed: Vec<_> = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Completed)
            .collect();
        (pending, in_progress, completed)
    }

    /// Scroll up by SCROLL_STEP lines
    pub fn scroll_up(&mut self) {
        let max_scroll = self.max_scroll();
        if self.scroll_offset < max_scroll {
            self.scroll_offset = (self.scroll_offset + SCROLL_STEP).min(max_scroll);
        }
        // Mark as intentionally at top if we've scrolled to max
        if self.scroll_offset >= max_scroll {
            self.intentionally_at_top = true;
        }
        self.maybe_load_more_history();
    }

    /// Scroll down by SCROLL_STEP lines
    pub fn scroll_down(&mut self) {
        if self.scroll_offset > 0 {
            self.scroll_offset = self.scroll_offset.saturating_sub(SCROLL_STEP);
            // No longer at top when scrolling down
            self.intentionally_at_top = false;
        }
    }

    /// Mouse wheel scroll up (slower than keyboard - 8 events = 1 line)
    pub fn mouse_scroll_up(&mut self) {
        self.mouse_scroll_accumulator += 1;
        if self.mouse_scroll_accumulator >= 8 {
            self.mouse_scroll_accumulator = 0;
            self.scroll_up();
        }
    }

    /// Mouse wheel scroll down (slower than keyboard - 8 events = 1 line)
    pub fn mouse_scroll_down(&mut self) {
        self.mouse_scroll_accumulator += 1;
        if self.mouse_scroll_accumulator >= 8 {
            self.mouse_scroll_accumulator = 0;
            self.scroll_down();
        }
    }

    /// Resize sidebar to place the divider at `mouse_x` given total terminal width.
    ///
    /// Clamps the resulting percentage to 20–60% so both panels remain usable.
    pub fn resize_sidebar_to(&mut self, mouse_x: u16, terminal_width: u16) {
        if terminal_width == 0 {
            return;
        }
        let pct = (mouse_x as u32 * 100 / terminal_width as u32).clamp(20, 60) as u16;
        self.sidebar_width_pct = pct;
        // Invalidate message render cache since layout changed
        self.message_render_cache = None;
    }

    /// Page up
    pub fn page_up(&mut self) {
        let page_size = self.visible_height.saturating_sub(2);
        let max_scroll = self.max_scroll();
        self.scroll_offset = (self.scroll_offset + page_size).min(max_scroll);
        // Mark as intentionally at top if we've paged to max
        if self.scroll_offset >= max_scroll {
            self.intentionally_at_top = true;
        }
        self.maybe_load_more_history();
    }

    /// Page down
    pub fn page_down(&mut self) {
        let page_size = self.visible_height.saturating_sub(2);
        self.scroll_offset = self.scroll_offset.saturating_sub(page_size);
        // No longer at top when paging down
        self.intentionally_at_top = false;
    }

    /// Scroll to top (oldest messages)
    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = self.max_scroll();
        self.intentionally_at_top = true;
        self.maybe_load_more_history();
    }

    /// Scroll to bottom (newest messages)
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.intentionally_at_top = false;
    }

    /// Cycle focus between panes: Board → Chat → InputBar → (Thread if open) → Board
    pub fn cycle_focus(&mut self) {
        self.focused_pane = match self.focused_pane {
            FocusedPane::Board => FocusedPane::Chat,
            FocusedPane::Chat => FocusedPane::InputBar,
            FocusedPane::InputBar => {
                if self.thread_parent_id.is_some() {
                    FocusedPane::Thread
                } else {
                    FocusedPane::Board
                }
            }
            FocusedPane::Thread => FocusedPane::Board,
        };
    }

    /// Open a thread view for the given parent message ID.
    ///
    /// Finds the parent message in the current messages, collects all existing
    /// thread replies, and switches focus to the Thread pane.
    /// Does nothing if the parent message ID is not found in loaded messages.
    pub fn open_thread(&mut self, parent_id: &str) {
        // Verify parent message exists in loaded messages
        let parent_exists = self.messages.iter().any(|m| m.id == parent_id);
        if !parent_exists {
            return;
        }

        self.thread_parent_id = Some(parent_id.to_string());

        // Collect existing thread replies from loaded messages
        self.thread_messages = self
            .messages
            .iter()
            .filter(|m| m.thread_parent_id.as_deref() == Some(parent_id))
            .cloned()
            .collect();

        self.thread_input_text.clear();
        self.thread_input_cursor = 0;
        self.focused_pane = FocusedPane::Thread;
    }

    /// Close the thread view and return focus to the main input bar.
    pub fn close_thread(&mut self) {
        self.thread_parent_id = None;
        self.thread_messages.clear();
        self.thread_input_text.clear();
        self.thread_input_cursor = 0;
        self.thread_input_area = None;
        self.focused_pane = FocusedPane::InputBar;
    }

    /// Post a thread reply message to the channel via daemon RPC with fallback.
    ///
    /// Similar to `post_message` but includes `thread_parent_id` so the message
    /// is recorded as a thread reply.
    ///
    /// Returns `true` if the message was successfully posted.
    pub fn post_thread_reply(
        &mut self,
        message: &str,
        sender: &str,
        channel_name: Option<&str>,
        thread_parent_id: &str,
    ) -> bool {
        use crate::client::DaemonClient;
        use midtown::{Message, MessageType};

        // In test mode, skip daemon communication to avoid side effects
        if self.test_mode {
            #[cfg(test)]
            {
                self.last_posted_channel = channel_name.map(|s| s.to_string());
            }

            if let Some(ref channel) = self.channel {
                let mut msg = Message::new(sender, message, MessageType::Text);
                msg.channel = channel_name.map(|s| s.to_string());
                msg.thread_parent_id = Some(thread_parent_id.to_string());
                return channel.send(&msg).is_ok();
            }
            return false;
        }

        // Try daemon RPC first with thread_parent_id
        let daemon_result = DaemonClient::connect().and_then(|client| {
            client.channel_post_as(message, sender, channel_name, Some(thread_parent_id))
        });

        if daemon_result.is_ok() {
            return true;
        }

        // Fall back to direct channel write
        if let Some(ref channel) = self.channel {
            let mut msg = Message::new(sender, message, MessageType::Text);
            msg.channel = channel_name.map(|s| s.to_string());
            msg.thread_parent_id = Some(thread_parent_id.to_string());
            channel.send(&msg).is_ok()
        } else {
            false
        }
    }

    /// Build the ordered list of selectable items in the board
    pub fn build_board_selections(&self) -> Vec<BoardSelection> {
        use std::collections::BTreeMap;

        let mut selections = Vec::new();

        // Use available_channels (already filtered by show_archived_channels)
        // This ensures navigation matches what's rendered in draw_board_panel
        let main_channel = self
            .available_channels
            .first()
            .map(|c| c.name.as_str())
            .unwrap_or("midtown");

        // Group tasks by channel
        let mut tasks_by_channel: BTreeMap<String, Vec<&KanbanTask>> = BTreeMap::new();
        let (pending, in_progress, _completed) = self.tasks_by_status();

        for task in in_progress.iter().chain(pending.iter()) {
            let channel_key = task.channel.as_deref().unwrap_or(main_channel).to_string();
            tasks_by_channel.entry(channel_key).or_default().push(task);
        }

        // Build selection list: include all available channels (not just those with tasks)
        // and add tasks under each channel
        for channel_info in &self.available_channels {
            // Add channel header
            selections.push(BoardSelection::Channel(channel_info.name.clone()));
            // Add tasks under this channel (if any)
            if let Some(tasks) = tasks_by_channel.get(&channel_info.name) {
                for task in tasks {
                    selections.push(BoardSelection::Task(
                        channel_info.name.clone(),
                        task.id.clone(),
                    ));
                }
            }
        }

        selections
    }

    /// Navigate board selection up
    pub fn board_selection_up(&mut self) {
        let selections = self.build_board_selections();
        if selections.is_empty() {
            return;
        }

        if let Some(ref current) = self.board_selection {
            // Find current position and move up
            if let Some(pos) = selections.iter().position(|s| s == current)
                && pos > 0
            {
                self.board_selection = Some(selections[pos - 1].clone());
                self.update_selected_channel();
            }
        } else {
            // No selection - select the last item
            self.board_selection = selections.last().cloned();
            self.update_selected_channel();
        }
    }

    /// Navigate board selection down
    pub fn board_selection_down(&mut self) {
        let selections = self.build_board_selections();
        if selections.is_empty() {
            return;
        }

        if let Some(ref current) = self.board_selection {
            // Find current position and move down
            if let Some(pos) = selections.iter().position(|s| s == current)
                && pos < selections.len() - 1
            {
                self.board_selection = Some(selections[pos + 1].clone());
                self.update_selected_channel();
            }
        } else {
            // No selection - select the first item
            self.board_selection = selections.first().cloned();
            self.update_selected_channel();
        }
    }

    /// Update the selected channel when a board selection changes
    pub fn update_selected_channel(&mut self) {
        if let Some(ref selection) = self.board_selection {
            let new_channel = match selection {
                BoardSelection::Channel(ch) => ch.clone(),
                BoardSelection::Task(ch, _) => ch.clone(),
            };

            // Only reload messages if the channel actually changed
            if new_channel != self.selected_channel {
                // Determine if the new channel is archived by checking directory layout.
                // A channel is archived if the .archived directory exists AND the
                // active directory does NOT exist. If both exist, the active one wins.
                let channel_repo =
                    midtown::paths::detect_repo_name().unwrap_or_else(|| "default".to_string());
                let base_dir = midtown::paths::projects_dir_for_repo(&channel_repo);
                let channels_dir = base_dir.join("channels");
                let has_active = channels_dir
                    .join(&new_channel)
                    .join("history")
                    .join("current.jsonl")
                    .exists();
                let has_archived = channels_dir
                    .join(format!("{}.archived", &new_channel))
                    .join("history")
                    .join("current.jsonl")
                    .exists();
                self.selected_channel_archived = has_archived && !has_active;

                self.selected_channel = new_channel;
                self.load_channel_messages();
            }
        }
    }

    /// Load messages from the currently selected channel
    fn load_channel_messages(&mut self) {
        #[cfg(test)]
        {
            self.load_channel_messages_called = true;
        }
        let channel_repo =
            midtown::paths::detect_repo_name().unwrap_or_else(|| "default".to_string());
        let base_dir = midtown::paths::projects_dir_for_repo(&channel_repo);

        // Try to open the channel file, using the correct method for archived channels.
        // Channel::new() creates files eagerly, so we must use open_archived() for
        // archived channels to avoid creating ghost .jsonl files.
        let channel_result = if self.selected_channel_archived {
            midtown::Channel::open_archived(&base_dir, &self.selected_channel)
        } else {
            midtown::Channel::new(&base_dir, &self.selected_channel)
        };
        if let Ok(channel) = channel_result {
            // Load last N messages
            if let Ok((messages, start_pos)) = channel.read_last_n_messages(INITIAL_MESSAGE_COUNT) {
                self.messages = VecDeque::from(messages);
                self.history_start_position = start_pos;
                self.history_fully_loaded = start_pos == 0;
                self.scroll_offset = 0;

                // Update the channel reference for future refresh
                self.channel = Some(channel);

                // Set cursor to end for new messages
                if let Some(ref ch) = self.channel {
                    let _ = ch.set_cursor_to_end("chat-tui");
                }
            }
        }
    }

    /// Maximum scroll offset
    fn max_scroll(&self) -> usize {
        self.messages.len().saturating_sub(self.visible_height)
    }

    /// Clamp scroll_offset to valid bounds.
    ///
    /// This should be called after visible_height changes (e.g., when the kanban
    /// board resizes) to prevent scroll_offset from exceeding max_scroll, which
    /// could cause unexpected behavior in is_at_max_scroll().
    ///
    /// When clamping occurs, intentionally_at_top is cleared because the user
    /// didn't explicitly scroll to the new position.
    pub fn clamp_scroll_offset(&mut self) {
        let max = self.max_scroll();
        if self.scroll_offset > max {
            self.scroll_offset = max;
            // Clear intentional flag since we're being forced to a new position
            self.intentionally_at_top = false;
        }
    }

    /// Check if we're intentionally viewing oldest messages (user scrolled to top).
    ///
    /// Used by the UI to determine line truncation strategy:
    /// - When true: show FIRST N lines (oldest content at top of visible area)
    /// - When false: show LAST N lines (smooth scrolling, current view at bottom)
    ///
    /// This distinguishes intentional "view history from beginning" (Home/g key)
    /// from scroll_offset accidentally exceeding max_scroll due to visible_height
    /// changes (e.g., kanban board resizing).
    pub fn is_at_max_scroll(&self) -> bool {
        let max = self.max_scroll();
        // Only consider "at max" if user intentionally scrolled there
        // AND we're actually at or beyond max scroll position
        max > 0 && self.intentionally_at_top && self.scroll_offset >= max
    }

    /// Check if user is near the top of loaded messages
    fn is_near_top(&self) -> bool {
        let max = self.max_scroll();
        // Consider "near top" if within 10 messages of the oldest loaded
        self.scroll_offset >= max.saturating_sub(10)
    }

    /// Load more history if user scrolls near the top
    fn maybe_load_more_history(&mut self) {
        // Don't load more if we've reached the cap or already have all history
        if self.history_fully_loaded
            || !self.is_near_top()
            || self.messages.len() >= MAX_LOADED_MESSAGES
        {
            return;
        }

        // Calculate how many more messages we can load without exceeding the cap
        let room_for = MAX_LOADED_MESSAGES.saturating_sub(self.messages.len());
        if room_for == 0 {
            return;
        }
        let load_count = room_for.min(INITIAL_MESSAGE_COUNT);

        if let Some(ref channel) = self.channel
            && let Ok((older_messages, new_start)) =
                channel.read_messages_before_position(self.history_start_position, load_count)
        {
            if !older_messages.is_empty() {
                let added = older_messages.len();
                // Prepend older messages to the beginning using VecDeque's O(1) push_front
                // Iterate in reverse so oldest message ends up at front
                for msg in older_messages.into_iter().rev() {
                    self.messages.push_front(msg);
                }

                // Adjust scroll offset to keep viewing the same messages
                self.scroll_offset += added;

                self.history_start_position = new_start;
            }
            if new_start == 0 {
                self.history_fully_loaded = true;
            }
        }
    }

    /// Get the channel file path for file watching
    pub fn channel_file_path(&self) -> Option<std::path::PathBuf> {
        self.channel
            .as_ref()
            .map(|c| c.channel_file_path().to_path_buf())
    }

    /// Post a message to the channel with fallback.
    ///
    /// Tries daemon RPC first (preferred - allows daemon to nudge lead),
    /// then falls back to direct channel write if daemon is unavailable.
    ///
    /// In test mode, skips daemon RPC to avoid side effects on the live system.
    ///
    /// Returns `true` if the message was successfully posted via either path.
    pub fn post_message(
        &mut self,
        message: &str,
        sender: &str,
        channel_name: Option<&str>,
    ) -> bool {
        use crate::client::DaemonClient;
        use midtown::{Message, MessageType};

        // In test mode, skip daemon communication to avoid side effects
        if self.test_mode {
            // Capture the channel argument for test verification
            #[cfg(test)]
            {
                self.last_posted_channel = channel_name.map(|s| s.to_string());
            }

            // Test mode: only try channel write if channel is available
            if let Some(ref channel) = self.channel {
                let mut msg = Message::new(sender, message, MessageType::Text);
                msg.channel = channel_name.map(|s| s.to_string());
                return channel.send(&msg).is_ok();
            }
            return false;
        }

        // Try daemon RPC first (preferred path - allows daemon to nudge lead)
        // Note: DaemonClient::connect() is synchronous with a 15s timeout.
        // This can freeze the UI if the daemon is unresponsive, but the
        // direct channel write fallback mitigates this by ensuring messages
        // are delivered even when the daemon is down. Making this async would
        // require restructuring the entire event loop.
        let daemon_result = DaemonClient::connect()
            .and_then(|client| client.channel_post_as(message, sender, channel_name, None));

        // If daemon RPC succeeds, we're done
        if daemon_result.is_ok() {
            return true;
        }

        // Fall back to direct channel write
        if let Some(ref channel) = self.channel {
            let mut msg = Message::new(sender, message, MessageType::Text);
            msg.channel = channel_name.map(|s| s.to_string());
            channel.send(&msg).is_ok()
        } else {
            false
        }
    }

    /// Deliver a pending clipboard image to the appropriate lead session.
    ///
    /// Sends a raw Ctrl+V byte (\x16) to the lead or channel-lead PTY via the
    /// headed intercom. Claude's own Ctrl+V handler reads /tmp/claude_cli_latest_screenshot.png
    /// (which must already be saved) and attaches the image to the conversation.
    ///
    /// Returns `true` if the nudge was enqueued, `false` on error.
    #[allow(dead_code)] // Called in Task 3+ when Ctrl+V paste handler invokes image delivery
    pub fn send_image_to_lead(&mut self) -> bool {
        use crate::client::DaemonClient;

        let target_session = self.image_target_session();

        let result = DaemonClient::connect()
            .and_then(|client| client.headed_enqueue_ctrl_v(&target_session));

        result.is_ok()
    }

    /// Determine which session to send the image to based on the current channel.
    ///
    /// If viewing a topic channel that has an active channel lead, send to that session.
    /// Otherwise send to the main "lead" session.
    #[allow(dead_code)] // Called by send_image_to_lead() in Task 3+
    fn image_target_session(&self) -> String {
        let channel = &self.selected_channel;
        if channel != "main"
            && !channel.is_empty()
            && self.channel_lead_names.iter().any(|n| n == channel)
        {
            return channel.clone();
        }
        "lead".to_string()
    }

    /// Validate that a channel name is safe for use as a filesystem path component.
    ///
    /// Rejects names containing path separators, traversal sequences, or other
    /// filesystem-unsafe characters that could escape the channels/ directory.
    fn is_valid_channel_name(name: &str) -> bool {
        if name.is_empty() {
            return false;
        }

        // Reject path separators and traversal sequences
        if name.contains('/') || name.contains('\\') || name.contains("..") {
            return false;
        }

        // Reject null bytes (filesystem-unsafe on all platforms)
        if name.contains('\0') {
            return false;
        }

        // Only allow alphanumeric, hyphens, underscores, and dots (not leading)
        name.chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
            && !name.starts_with('.')
    }

    /// Create a new channel and switch to it
    ///
    /// Returns `true` if the channel was successfully created (or already exists),
    /// `false` on error. Rejects channel names with path-unsafe characters.
    pub fn create_channel(&mut self, channel_name: &str) -> bool {
        // Validate channel name to prevent path traversal
        if !Self::is_valid_channel_name(channel_name) {
            return false;
        }

        let channel_repo =
            midtown::paths::detect_repo_name().unwrap_or_else(|| "default".to_string());
        let base_dir = midtown::paths::projects_dir_for_repo(&channel_repo);

        // Create the channel (this is idempotent - Channel::new creates the file if it doesn't exist)
        match midtown::Channel::new(&base_dir, channel_name) {
            Ok(_) => {
                // Switch to the newly created channel and load its messages
                self.selected_channel = channel_name.to_string();
                self.load_channel_messages();
                true
            }
            Err(_) => false,
        }
    }

    /// Get messages visible in the current scroll position
    pub fn visible_messages(&mut self) -> &[Message] {
        let total = self.messages.len();
        if total == 0 {
            return &[];
        }

        // scroll_offset=0 means we show the most recent messages (end of list)
        // Higher scroll_offset means we show older messages
        let end = total.saturating_sub(self.scroll_offset);
        let start = end.saturating_sub(self.visible_height);

        // VecDeque is a ring buffer; make_contiguous ensures we can return a slice
        let slice = self.messages.make_contiguous();
        &slice[start..end]
    }

    /// Get the cached current_tasks map, rebuilding if tasks have changed.
    /// This avoids rebuilding the HashMap on every frame.
    pub fn current_tasks(&mut self) -> &HashMap<String, String> {
        // Compute a simple hash of task state to detect changes
        let new_hash = self.compute_tasks_hash();
        if new_hash != self.tasks_cache_hash {
            self.rebuild_current_tasks_cache();
            self.tasks_cache_hash = new_hash;
        }
        &self.current_tasks_cache
    }

    /// Compute a cache key for message rendering.
    ///
    /// This captures all inputs that affect the rendered output of draw_chat_messages():
    /// scroll position, message count, terminal width, selection mode, last message ID
    /// as a proxy for content changes, task state, mermaid render state, and the
    /// indicator height (which affects the message area size via the layout constraint).
    pub fn message_cache_key(&self, width: u16, height: u16) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        self.scroll_offset.hash(&mut hasher);
        self.messages.len().hash(&mut hasher);
        width.hash(&mut hasher);
        height.hash(&mut hasher);
        self.selection_mode.hash(&mut hasher);
        // Hash last message ID as proxy for content changes
        if let Some(last) = self.messages.back() {
            last.id.hash(&mut hasher);
        }
        // Hash task cache state (current_tasks affects sender labels)
        self.tasks_cache_hash.hash(&mut hasher);
        // Hash mermaid render state — when a diagram finishes background rendering,
        // the completed count changes and we need to re-render to show the diagram
        // instead of the "rendering..." placeholder.
        self.mermaid_cache.completed_count().hash(&mut hasher);
        // Hash thread state — opening/closing thread changes the chat area width,
        // and thread reply counts affect reply indicators displayed after messages.
        if let Some(ref thread_id) = self.thread_parent_id {
            thread_id.hash(&mut hasher);
        }
        self.thread_messages.len().hash(&mut hasher);
        // Hash selected channel — changing channels changes which agent's indicator
        // is shown, which may affect indicator_height and thus the message area size.
        self.selected_channel.hash(&mut hasher);
        // Hash the indicator height — as completed entries age out, indicator_height changes,
        // which changes the message area size via the layout constraint. Without this,
        // the cache produces stale line counts when the indicator collapses.
        let agent_key = if self.selected_channel == "main" || self.selected_channel == "midtown" {
            "lead"
        } else {
            self.selected_channel.as_str()
        };
        self.visible_tool_entries(agent_key).len().hash(&mut hasher);
        // Hash channel_lead_thinking for the current agent — the optimistic thinking state
        // also changes lead_indicator_height (0 -> 1), affecting the message area layout.
        let is_thinking = self
            .channel_lead_thinking
            .get(agent_key)
            .map(|t| t.elapsed() < CHANNEL_LEAD_THINKING_TIMEOUT)
            .unwrap_or(false);
        is_thinking.hash(&mut hasher);
        hasher.finish()
    }

    /// Compute a hash of the relevant task state for cache invalidation
    fn compute_tasks_hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        for task in &self.tasks {
            if task.status == TaskStatus::InProgress {
                task.id.hash(&mut hasher);
                task.owner.hash(&mut hasher);
                task.subject.hash(&mut hasher);
            }
        }
        hasher.finish()
    }

    /// Rebuild the current_tasks cache from task data
    fn rebuild_current_tasks_cache(&mut self) {
        self.current_tasks_cache.clear();
        for task in &self.tasks {
            if task.status == TaskStatus::InProgress
                && let Some(ref owner) = task.owner
            {
                self.current_tasks_cache
                    .insert(owner.to_lowercase(), task.subject.clone());
            }
        }
    }

    /// Refresh the list of available channels from the daemon.
    ///
    /// This fetches channels from the daemon's RPC interface (same as web UI),
    /// ensuring TUI and web UI show the same channel list.
    pub fn refresh_available_channels(&mut self) {
        // Skip in test mode to avoid daemon communication
        if self.test_mode {
            return;
        }

        // Try daemon RPC first (ensures parity with web UI)
        if let Ok(client) = crate::client::DaemonClient::connect()
            && let Ok(crate::cli::Response::Json { value }) =
                client.channel_list(self.show_archived_channels)
            && let Some(channels_value) = value.get("channels")
            && let Ok(channels) =
                serde_json::from_value::<Vec<midtown::ChannelInfo>>(channels_value.clone())
        {
            self.available_channels = channels;
            return;
        }

        // Fallback to direct filesystem access if daemon is unavailable
        let base_dir = match &self.channel {
            Some(ch) => ch.base_dir().to_path_buf(),
            None => return,
        };

        if let Ok(channels) = Channel::list(
            &base_dir,
            self.show_archived_channels,
            Some(&self.project_name),
        ) {
            self.available_channels = channels;
        }
    }

    /// Calculate unread message counts for all channels
    ///
    /// For each channel, compares the total message count with the cursor position
    /// for the "chat-tui" agent to determine how many messages are unread.
    pub fn refresh_unread_counts(&mut self) {
        self.channel_unread_counts.clear();

        // Get the base directory from the current channel if available
        let base_dir = match &self.channel {
            Some(ch) => ch.base_dir().to_path_buf(),
            None => return, // No channel, can't calculate unread counts
        };

        // List all available channels (based on current filter setting)
        let channels = match Channel::list(
            &base_dir,
            self.show_archived_channels,
            Some(&self.project_name),
        ) {
            Ok(list) => list,
            Err(_) => return, // Can't read channel list, skip
        };

        for channel_info in channels {
            // Open the channel (use different method for archived channels)
            let channel = if channel_info.is_archived {
                match Channel::open_archived(&base_dir, &channel_info.name) {
                    Ok(ch) => ch,
                    Err(_) => continue, // Skip channels we can't open
                }
            } else {
                match Channel::new(&base_dir, &channel_info.name) {
                    Ok(ch) => ch,
                    Err(_) => continue, // Skip channels we can't open
                }
            };

            // Get total message count
            let total_count = match channel.message_count() {
                Ok(count) => count,
                Err(_) => continue, // Skip channels we can't read
            };

            // Calculate unread count:
            // Load the cursor without updating it, then count messages from that position
            let cursor =
                match midtown::Cursor::load_or_create(&base_dir, &channel_info.name, "chat-tui") {
                    Ok(c) => c,
                    Err(_) => {
                        // If we can't load cursor, assume all messages are unread
                        if total_count > 0 {
                            self.channel_unread_counts
                                .insert(channel_info.name.clone(), total_count);
                        }
                        continue;
                    }
                };

            // Read all messages and count how many come after the cursor position
            let all_messages = match channel.read_all() {
                Ok(msgs) => msgs,
                Err(_) => continue,
            };

            // Count messages that come after the cursor's last_message_id
            let unread_count = if let Some(ref last_id) = cursor.last_message_id {
                // Find the position of the last read message
                let last_read_idx = all_messages.iter().position(|m| &m.id == last_id);
                match last_read_idx {
                    Some(idx) => all_messages.len().saturating_sub(idx + 1),
                    None => all_messages.len(), // Cursor points to non-existent message, all are unread
                }
            } else {
                // Cursor has never been updated, all messages are unread
                all_messages.len()
            };

            if unread_count > 0 {
                self.channel_unread_counts
                    .insert(channel_info.name, unread_count);
            }
        }
    }

    /// Detect autocomplete trigger and update autocomplete state
    ///
    /// Scans backward from the cursor position to find trigger characters (@, #, !)
    /// that are preceded by whitespace or start of line.
    pub fn detect_autocomplete_trigger(&mut self) {
        let cursor_pos = self.input_cursor;
        let text = &self.input_text;

        // Check for "/thread " prefix — slash command autocomplete
        if text.starts_with("/thread ") && cursor_pos >= 8 {
            let query = text[8..].to_string();
            self.autocomplete.trigger_type = Some('/');
            self.autocomplete.query = query.clone();
            self.autocomplete.trigger_start_pos = 0;
            self.autocomplete.items = self.get_thread_items(&query.to_lowercase());
            self.autocomplete.selected_index = 0;
            self.autocomplete.show = !self.autocomplete.items.is_empty();
            return;
        }

        // Look backward from cursor to find trigger character
        let mut trigger_pos: Option<usize> = None;
        let mut trigger_char: Option<char> = None;

        // Convert character indices to byte positions
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        if cursor_pos > chars.len() {
            self.autocomplete.show = false;
            return;
        }

        // Find byte position of cursor
        let cursor_byte_pos = if cursor_pos < chars.len() {
            chars[cursor_pos].0
        } else {
            text.len()
        };

        // Scan backward from cursor
        for i in (0..cursor_pos).rev() {
            let (byte_idx, ch) = chars[i];
            let prev_char = if i > 0 { Some(chars[i - 1].1) } else { None };

            // Check if this is a trigger character preceded by whitespace or start of line
            if matches!(ch, '@' | '#' | '!')
                && (prev_char.is_none() || prev_char == Some(' ') || prev_char == Some('\n'))
            {
                trigger_pos = Some(byte_idx);
                trigger_char = Some(ch);
                break;
            }

            // Stop if we hit a space or newline (no trigger found in this word)
            if ch == ' ' || ch == '\n' {
                break;
            }
        }

        if let (Some(trigger_byte_pos), Some(trigger)) = (trigger_pos, trigger_char) {
            // Extract query string between trigger and cursor
            let query = text[trigger_byte_pos + 1..cursor_byte_pos].to_string();

            // Update autocomplete state
            self.autocomplete.trigger_type = Some(trigger);
            self.autocomplete.query = query.clone();
            self.autocomplete.trigger_start_pos = trigger_byte_pos;
            self.autocomplete.items = self.get_autocomplete_items(trigger, &query);
            self.autocomplete.selected_index = 0;
            self.autocomplete.show = !self.autocomplete.items.is_empty();
        } else {
            self.autocomplete.show = false;
        }
    }

    /// Get autocomplete items for the given trigger and query
    fn get_autocomplete_items(&self, trigger: char, query: &str) -> Vec<AutocompleteItem> {
        let query_lower = query.to_lowercase();

        match trigger {
            '@' => self.get_mention_items(&query_lower),
            '#' => self.get_channel_items(&query_lower),
            '!' => self.get_task_items(&query_lower),
            _ => Vec::new(),
        }
    }

    /// Get @mention autocomplete items (coworkers + lead)
    fn get_mention_items(&self, query: &str) -> Vec<AutocompleteItem> {
        let mut items = Vec::new();

        // Add "lead" first
        if "lead".starts_with(query) {
            items.push(AutocompleteItem {
                value: "@lead".to_string(),
                description: None,
            });
        }

        // In test mode, use self.coworkers instead of daemon
        // Skip "lead" since it's already added above
        if self.test_mode {
            for cw in &self.coworkers {
                if cw.name.eq_ignore_ascii_case("lead") {
                    continue;
                }
                if cw.name.to_lowercase().starts_with(query) {
                    // Look up current task from the tasks cache
                    let current_task = self
                        .current_tasks_cache
                        .get(&cw.name.to_lowercase())
                        .cloned();
                    items.push(AutocompleteItem {
                        value: format!("@{}", cw.name),
                        description: current_task,
                    });
                }
            }
        } else {
            // Add coworkers from cached list (populated from daemon status)
            // Skip "lead" since it's already added above
            for cw in &self.coworkers {
                if cw.name.eq_ignore_ascii_case("lead") {
                    continue;
                }
                if cw.name.to_lowercase().starts_with(query) {
                    // Look up current task from the tasks cache
                    let current_task = self
                        .current_tasks_cache
                        .get(&cw.name.to_lowercase())
                        .cloned();
                    items.push(AutocompleteItem {
                        value: format!("@{}", cw.name),
                        description: current_task,
                    });
                }
            }
        }

        items
    }

    /// Get #channel autocomplete items
    fn get_channel_items(&self, query: &str) -> Vec<AutocompleteItem> {
        let mut items = Vec::new();

        // Get available channels from the channel system
        if let Some(ref channel) = self.channel {
            let base_dir = channel.base_dir();
            // For autocomplete, include archived channels so users can reference them
            if let Ok(channels) = Channel::list(base_dir, true, Some(&self.project_name)) {
                for channel_info in channels {
                    if channel_info.name.to_lowercase().starts_with(query) {
                        items.push(AutocompleteItem {
                            value: format!("#{}", channel_info.name),
                            description: None,
                        });
                    }
                }
            }
        }

        items
    }

    /// Get !task autocomplete items.
    /// When query is empty, shows in_progress tasks first (most useful default).
    fn get_task_items(&self, query: &str) -> Vec<AutocompleteItem> {
        let to_item = |task: &KanbanTask| AutocompleteItem {
            value: format!("!{}", task.id),
            description: Some(task.subject.clone()),
        };

        if query.is_empty() {
            // Show in_progress tasks first, then pending
            let mut items: Vec<AutocompleteItem> = self
                .tasks
                .iter()
                .filter(|t| matches!(t.status, TaskStatus::InProgress))
                .map(to_item)
                .collect();
            items.extend(
                self.tasks
                    .iter()
                    .filter(|t| !matches!(t.status, TaskStatus::InProgress))
                    .map(to_item),
            );
            items
        } else {
            self.tasks
                .iter()
                .filter(|task| {
                    task.id.starts_with(query) || task.subject.to_lowercase().starts_with(query)
                })
                .map(to_item)
                .collect()
        }
    }

    /// Get /thread autocomplete items — recent top-level messages from the current channel.
    /// Shows most recent messages first, limited to 20 items.
    fn get_thread_items(&self, query: &str) -> Vec<AutocompleteItem> {
        self.messages
            .iter()
            .rev()
            .filter(|m| m.thread_parent_id.is_none())
            .filter(|m| {
                if query.is_empty() {
                    true
                } else {
                    m.content.to_lowercase().contains(query)
                        || m.from.to_lowercase().contains(query)
                }
            })
            .take(20)
            .map(|m| {
                let truncated = if m.content.len() > 50 {
                    format!("{}...", &m.content[..m.content.floor_char_boundary(50)])
                } else {
                    m.content.clone()
                };
                AutocompleteItem {
                    value: m.id.clone(),
                    description: Some(format!("{}: {}", m.from, truncated)),
                }
            })
            .collect()
    }

    /// Insert the selected autocomplete item into the input text.
    /// For `/thread` autocomplete (trigger '/'), opens the thread instead of inserting text.
    pub fn insert_autocomplete_item(&mut self) {
        if !self.autocomplete.show
            || self.autocomplete.selected_index >= self.autocomplete.items.len()
        {
            return;
        }

        let item = &self.autocomplete.items[self.autocomplete.selected_index];
        let value = item.value.clone(); // Clone to avoid borrow issues

        // For /thread autocomplete, open the thread and clear input
        if self.autocomplete.trigger_type == Some('/') {
            self.autocomplete.show = false;
            self.input_text.clear();
            self.input_cursor = 0;
            self.open_thread(&value);
            return;
        }

        // Convert cursor position (character index) to byte position
        let chars: Vec<(usize, char)> = self.input_text.char_indices().collect();
        let cursor_byte_pos = if self.input_cursor < chars.len() {
            chars[self.input_cursor].0
        } else {
            self.input_text.len()
        };

        // Extract parts before trigger and after cursor
        let before_trigger = self.input_text[..self.autocomplete.trigger_start_pos].to_string();
        let after_cursor = self.input_text[cursor_byte_pos..].to_string();

        // Construct new input text with selected value + space
        self.input_text = format!("{}{} {}", before_trigger, value, after_cursor);

        // Update cursor position (in character indices)
        let new_cursor_chars = format!("{}{} ", before_trigger, value).chars().count();
        self.input_cursor = new_cursor_chars;

        // Hide autocomplete
        self.autocomplete.show = false;
    }

    /// Navigate autocomplete selection up
    pub fn autocomplete_select_prev(&mut self) {
        if self.autocomplete.show && !self.autocomplete.items.is_empty() {
            if self.autocomplete.selected_index == 0 {
                self.autocomplete.selected_index = self.autocomplete.items.len() - 1;
            } else {
                self.autocomplete.selected_index -= 1;
            }
        }
    }

    /// Navigate autocomplete selection down
    pub fn autocomplete_select_next(&mut self) {
        if self.autocomplete.show && !self.autocomplete.items.is_empty() {
            self.autocomplete.selected_index =
                (self.autocomplete.selected_index + 1) % self.autocomplete.items.len();
        }
    }

    /// Dismiss autocomplete dropdown
    pub fn dismiss_autocomplete(&mut self) {
        self.autocomplete.show = false;
    }

    /// Toggle the channel switcher overlay (Ctrl+K)
    pub fn toggle_channel_switcher(&mut self) {
        if self.channel_switcher.show {
            // Hide it
            self.channel_switcher.show = false;
            self.channel_switcher.input.clear();
            self.channel_switcher.filtered_channels.clear();
            self.channel_switcher.selected_index = 0;
        } else {
            // Show it and populate with all channels
            self.channel_switcher.show = true;
            self.channel_switcher.input.clear();
            self.channel_switcher.selected_index = 0;
            self.update_channel_switcher_filter();
        }
    }

    /// Update the channel switcher input and filter
    pub fn channel_switcher_input(&mut self, c: char) {
        if !self.channel_switcher.show {
            return;
        }
        self.channel_switcher.input.push(c);
        self.update_channel_switcher_filter();
    }

    /// Backspace in the channel switcher input
    pub fn channel_switcher_backspace(&mut self) {
        if !self.channel_switcher.show {
            return;
        }
        self.channel_switcher.input.pop();
        self.update_channel_switcher_filter();
    }

    /// Update the filtered channel list based on current input
    fn update_channel_switcher_filter(&mut self) {
        let query = self.channel_switcher.input.to_lowercase();

        // Get all available channels from board selections
        let selections = self.build_board_selections();
        let mut channels = Vec::new();

        for selection in selections {
            if let BoardSelection::Channel(name) = selection {
                channels.push(name);
            }
        }

        // Filter channels by prefix match
        let filtered: Vec<ChannelSwitcherItem> = channels
            .into_iter()
            .filter(|name| query.is_empty() || name.to_lowercase().starts_with(&query))
            .map(|name| {
                let unread_count = self.channel_unread_counts.get(&name).copied().unwrap_or(0);
                ChannelSwitcherItem { name, unread_count }
            })
            .collect();

        self.channel_switcher.filtered_channels = filtered;

        // Reset selection if out of bounds
        if self.channel_switcher.selected_index >= self.channel_switcher.filtered_channels.len() {
            self.channel_switcher.selected_index = 0;
        }
    }

    /// Navigate channel switcher selection up
    pub fn channel_switcher_select_prev(&mut self) {
        if !self.channel_switcher.show || self.channel_switcher.filtered_channels.is_empty() {
            return;
        }
        if self.channel_switcher.selected_index == 0 {
            self.channel_switcher.selected_index =
                self.channel_switcher.filtered_channels.len() - 1;
        } else {
            self.channel_switcher.selected_index -= 1;
        }
    }

    /// Navigate channel switcher selection down
    pub fn channel_switcher_select_next(&mut self) {
        if !self.channel_switcher.show || self.channel_switcher.filtered_channels.is_empty() {
            return;
        }
        self.channel_switcher.selected_index = (self.channel_switcher.selected_index + 1)
            % self.channel_switcher.filtered_channels.len();
    }

    /// Select the currently highlighted channel and close the switcher
    pub fn channel_switcher_select(&mut self) {
        if !self.channel_switcher.show || self.channel_switcher.filtered_channels.is_empty() {
            return;
        }

        let selected_channel = self.channel_switcher.filtered_channels
            [self.channel_switcher.selected_index]
            .name
            .clone();

        // Switch to the selected channel
        let channel_repo =
            midtown::paths::detect_repo_name().unwrap_or_else(|| "default".to_string());
        let base_dir = midtown::paths::projects_dir_for_repo(&channel_repo);
        let channels_dir = base_dir.join("channels");
        let has_active = channels_dir
            .join(&selected_channel)
            .join("history")
            .join("current.jsonl")
            .exists();
        let has_archived = channels_dir
            .join(format!("{}.archived", &selected_channel))
            .join("history")
            .join("current.jsonl")
            .exists();
        self.selected_channel_archived = has_archived && !has_active;
        self.selected_channel = selected_channel.clone();

        // Update board selection to the selected channel
        self.board_selection = Some(BoardSelection::Channel(selected_channel));

        // Close the switcher
        self.channel_switcher.show = false;
        self.channel_switcher.input.clear();
        self.channel_switcher.filtered_channels.clear();
        self.channel_switcher.selected_index = 0;

        // Load messages from the newly selected channel
        self.load_channel_messages();
    }

    /// Dismiss the channel switcher without selecting
    pub fn dismiss_channel_switcher(&mut self) {
        self.channel_switcher.show = false;
        self.channel_switcher.input.clear();
        self.channel_switcher.filtered_channels.clear();
        self.channel_switcher.selected_index = 0;
    }

    /// Get the current spinner character without advancing the frame.
    /// Returns a braille spinner character (⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏).
    /// Frame advancement is time-based via `tick_spinner()`.
    pub fn spinner_char(&self) -> &'static str {
        const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        SPINNER_FRAMES[self.spinner_frame % SPINNER_FRAMES.len()]
    }

    /// Set optimistic thinking state for a topic channel after user submits a message.
    pub fn set_channel_lead_thinking(&mut self, channel: &str) {
        self.channel_lead_thinking
            .insert(channel.to_string(), std::time::Instant::now());
    }

    /// Clear optimistic thinking state for channels that now have InProgress tool activity.
    ///
    /// Only clears when InProgress entries (completed_at == None) exist, not stale completed
    /// entries. Completed entries are retained in tool_activity until they age out, so
    /// filtering on non-empty would prematurely clear the spinner.
    pub fn clear_channel_lead_thinking_for_in_progress(&mut self) {
        let channels_with_in_progress: Vec<String> = self
            .tool_activity
            .iter()
            .filter(|(_, entries)| entries.iter().any(|e| e.completed_at.is_none()))
            .map(|(ch, _)| ch.clone())
            .collect();
        for ch in channels_with_in_progress {
            self.channel_lead_thinking.remove(&ch);
        }
    }

    /// Returns the visible tool activity entries for the given agent, newest first.
    ///
    /// Applies 30-second age-out for completed (✓/✗) entries. In-progress (›) entries
    /// are always included. Returns at most 3 entries.
    pub fn visible_tool_entries(&self, agent_key: &str) -> Vec<&ToolActivityEntry> {
        const ACTIVITY_AGE_OUT: std::time::Duration = std::time::Duration::from_secs(30);
        let now = std::time::Instant::now();

        let entries = match self.tool_activity.get(agent_key) {
            Some(e) if !e.is_empty() => e,
            _ => return Vec::new(),
        };

        let mut visible: Vec<&ToolActivityEntry> = entries
            .iter()
            .filter(|entry| {
                match entry.completed_at {
                    None => true, // In-progress entries never age out
                    Some(completed_at) => now.duration_since(completed_at) < ACTIVITY_AGE_OUT,
                }
            })
            .collect();

        // Newest first: reverse the slice (most recent entries are appended last in the Vec).
        visible.reverse();
        // Cap at 3.
        visible.truncate(3);
        visible
    }

    /// Returns true if any spinner is currently visible (lead working, in-progress tool entries, or active coworkers).
    pub fn any_spinner_visible(&self) -> bool {
        self.lead_working
            || self
                .tool_activity
                .values()
                .any(|entries| entries.iter().any(|e| e.completed_at.is_none()))
            || self
                .coworkers
                .iter()
                .any(|cw| cw.phase.as_deref() != Some("idle") && cw.phase.is_some())
            || self
                .channel_lead_thinking
                .values()
                .any(|t| t.elapsed() < CHANNEL_LEAD_THINKING_TIMEOUT)
    }

    /// Advance the spinner frame if enough time has elapsed since the last tick.
    pub fn tick_spinner(&mut self) {
        const SPINNER_INTERVAL: Duration = Duration::from_millis(100);
        if self.spinner_last_tick.elapsed() >= SPINNER_INTERVAL {
            self.spinner_frame = self.spinner_frame.wrapping_add(1);
            self.spinner_last_tick = Instant::now();
        }
    }
}

/// Fetch tasks from Claude Code's task storage.
///
/// Reads tasks from `~/.claude/tasks/midtown-<repo>/` using the shared task list ID.
fn fetch_tasks() -> Vec<KanbanTask> {
    let mut tasks = Vec::new();

    // Get home directory
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return tasks,
    };

    // Use the shared task list ID (midtown-<repo>)
    let task_list_id = midtown::paths::task_list_id();

    // Read tasks from ~/.claude/tasks/midtown-<repo>/
    let tasks_dir = home.join(".claude").join("tasks").join(&task_list_id);
    let entries = match std::fs::read_dir(&tasks_dir) {
        Ok(e) => e,
        Err(_) => return tasks,
    };

    // Read each task file (*.json)
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json")
            && let Ok(content) = std::fs::read_to_string(&path)
            && let Ok(task_data) = serde_json::from_str::<serde_json::Value>(&content)
        {
            let id = task_data
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let subject = task_data
                .get("subject")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let owner = task_data
                .get("owner")
                .and_then(|v| v.as_str())
                .map(String::from);
            let status_str = task_data
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("pending");

            let status = match status_str {
                "in_progress" => TaskStatus::InProgress,
                "completed" => TaskStatus::Completed,
                _ => TaskStatus::Pending,
            };

            // Get file modification time as proxy for when status changed
            let modified_at = std::fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok())
                .map(DateTime::<Utc>::from);

            // Read channel assignment
            let channel = task_data
                .get("channel")
                .and_then(|v| v.as_str())
                .map(String::from);

            // Read blocked_by array
            let blocked_by = task_data
                .get("blockedBy")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            if !id.is_empty() {
                tasks.push(KanbanTask {
                    id,
                    subject,
                    owner,
                    status,
                    modified_at,
                    channel,
                    blocked_by,
                });
            }
        }
    }

    // Sort tasks by ID for consistent display
    tasks.sort_by(|a, b| {
        let a_num: i32 = a.id.parse().unwrap_or(i32::MAX);
        let b_num: i32 = b.id.parse().unwrap_or(i32::MAX);
        a_num.cmp(&b_num)
    });

    tasks
}

/// Fetch repo name with owner from gh CLI
fn fetch_repo_name() -> String {
    if let Ok(output) = std::process::Command::new("gh")
        .args(["repo", "view", "--json", "nameWithOwner"])
        .output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Ok(repo) = serde_json::from_str::<serde_json::Value>(&stdout)
            && let Some(name) = repo.get("nameWithOwner").and_then(|v| v.as_str())
        {
            return name.to_string();
        }
    }
    // Fallback to just directory name
    std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
        .unwrap_or_else(|| "repo".to_string())
}

/// Extract coworker name from PR body frontmatter.
///
/// Looks for `<!-- midtown: coworkername -->` in the PR body and extracts the coworker name.
/// Handles variations like `<!--midtown:name-->` or `<!-- midtown: name -->`.
fn extract_coworker_from_body(body: &str) -> Option<String> {
    // Look for midtown: pattern within an HTML comment
    // Handle both "<!-- midtown:" and "<!--midtown:" formats
    let marker = "midtown:";

    if let Some(marker_pos) = body.find(marker) {
        // Check that it's within an HTML comment (<!-- before it)
        let before = &body[..marker_pos];
        if !before.contains("<!--") {
            return None;
        }

        let after_marker = &body[marker_pos + marker.len()..];
        if let Some(end) = after_marker.find("-->") {
            let name = after_marker[..end].trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Fetch open PRs from GitHub using gh CLI
fn fetch_prs() -> Vec<KanbanPr> {
    let mut prs = Vec::new();

    // Build task map for task_id -> subject lookup (Task.id is String)
    let task_map: HashMap<String, String> = midtown::tasks::read_tasks()
        .into_iter()
        .map(|t| (t.id, t.subject))
        .collect();

    if let Ok(output) = std::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--json",
            "number,title,author,createdAt,body,statusCheckRollup,comments",
        ])
        .output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Ok(pr_list) = serde_json::from_str::<Vec<serde_json::Value>>(&stdout) {
            for pr in pr_list {
                let number = pr.get("number").and_then(|v| v.as_u64()).unwrap_or(0);
                let title = pr
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let github_author = pr
                    .get("author")
                    .and_then(|v| v.get("login"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let body = pr.get("body").and_then(|v| v.as_str()).unwrap_or("");
                // Use coworker name from body frontmatter if present, otherwise GitHub author
                let author = extract_coworker_from_body(body).unwrap_or(github_author);
                let created_at = pr
                    .get("createdAt")
                    .and_then(|v| v.as_str())
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(Utc::now);

                // Parse CI status from statusCheckRollup
                let ci_status = parse_ci_status_from_checks(
                    pr.get("statusCheckRollup")
                        .and_then(|v| v.as_array())
                        .map(|a| a.as_slice())
                        .unwrap_or(&[]),
                );

                // Extract reviewer from comments (look for review comment frontmatter)
                let (reviewer, reviewed_at) = extract_reviewer_from_comments(
                    pr.get("comments")
                        .and_then(|v| v.as_array())
                        .map(|a| a.as_slice())
                        .unwrap_or(&[]),
                );

                if number > 0 {
                    let review_posted = reviewer.is_some();
                    // Extract task info from PR title
                    let task_id = extract_task_id_from_pr_title(&title);
                    let task_name = task_id.and_then(|id| task_map.get(&id.to_string()).cloned());
                    prs.push(KanbanPr {
                        number,
                        title,
                        author,
                        created_at,
                        ci_status,
                        reviewer,
                        reviewed_at,
                        review_posted,
                        repo: None,
                        task_id,
                        task_name,
                        has_conflicts: false, // Local fetch doesn't check mergeable status
                    });
                }
            }
        }
    }

    prs
}

/// Extract reviewer name and timestamp from PR comments.
///
/// Looks for code review comments containing `<!-- midtown: name -->` frontmatter
/// or "Code Review by {name}" headers. Returns the first reviewer found and the
/// comment's creation timestamp.
fn extract_reviewer_from_comments(
    comments: &[serde_json::Value],
) -> (Option<String>, Option<DateTime<Utc>>) {
    for comment in comments {
        let body = comment.get("body").and_then(|v| v.as_str()).unwrap_or("");

        // Check if this looks like a review comment
        let is_review = body.contains("Code Review") || body.contains("Code review");
        if !is_review {
            continue;
        }

        // Try to extract reviewer name from frontmatter
        let reviewer_name = extract_coworker_from_body(body);

        // Fall back to "Code Review by {name}" pattern
        let reviewer_name = reviewer_name.or_else(|| extract_reviewer_from_header(body));

        if let Some(name) = reviewer_name {
            let created_at = comment
                .get("createdAt")
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc));
            return (Some(name), created_at);
        }
    }
    (None, None)
}

/// Extract reviewer name from "Code Review by {name}" or "## Code Review by {name}" header.
fn extract_reviewer_from_header(body: &str) -> Option<String> {
    for line in body.lines() {
        let trimmed = line.trim().trim_start_matches('#').trim();
        if let Some(rest) = trimmed
            .strip_prefix("Code Review by ")
            .or_else(|| trimmed.strip_prefix("Code review by "))
        {
            let name = rest.trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Parse CI status from GitHub statusCheckRollup array
fn parse_ci_status_from_checks(checks: &[serde_json::Value]) -> CiStatus {
    if checks.is_empty() {
        return CiStatus::Unknown;
    }

    let mut has_running = false;
    let mut has_failed = false;
    let mut has_passed = false;

    for check in checks {
        // Check runs have "status" and "conclusion" fields
        // Status contexts have "state" field
        let status = check.get("status").and_then(|v| v.as_str());
        let conclusion = check.get("conclusion").and_then(|v| v.as_str());
        let state = check.get("state").and_then(|v| v.as_str());

        // Handle check runs (GitHub Actions)
        if let Some(status) = status {
            match status {
                "IN_PROGRESS" | "QUEUED" | "WAITING" | "PENDING" => has_running = true,
                "COMPLETED" => match conclusion {
                    Some("SUCCESS") => has_passed = true,
                    Some("FAILURE") | Some("CANCELLED") | Some("TIMED_OUT") => has_failed = true,
                    _ => {}
                },
                _ => {}
            }
        }

        // Handle status contexts (external CI)
        if let Some(state) = state {
            match state {
                "PENDING" => has_running = true,
                "SUCCESS" => has_passed = true,
                "FAILURE" | "ERROR" => has_failed = true,
                _ => {}
            }
        }
    }

    // Priority: failed > running > passed > unknown
    if has_failed {
        CiStatus::Failed
    } else if has_running {
        CiStatus::Running
    } else if has_passed {
        CiStatus::Passed
    } else {
        CiStatus::Unknown
    }
}

/// Fetch merged PRs from GitHub using gh CLI (for Done column)
fn fetch_merged_prs() -> Vec<MergedPr> {
    let mut prs = Vec::new();

    if let Ok(output) = std::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--state",
            "merged",
            "--json",
            "number,title,mergedAt",
            "--limit",
            "10",
        ])
        .output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Ok(pr_list) = serde_json::from_str::<Vec<serde_json::Value>>(&stdout) {
            for pr in pr_list {
                let number = pr.get("number").and_then(|v| v.as_u64()).unwrap_or(0);
                let title = pr
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let merged_at = pr
                    .get("mergedAt")
                    .and_then(|v| v.as_str())
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(Utc::now);

                if number > 0 {
                    prs.push(MergedPr {
                        number,
                        title,
                        merged_at,
                        repo: None,
                    });
                }
            }
        }
    }

    // Sort by merged_at descending (most recent first)
    prs.sort_by(|a, b| b.merged_at.cmp(&a.merged_at));
    prs
}

type KanbanRpcResult = Option<(Vec<KanbanPr>, Vec<MergedPr>, Vec<(String, String)>)>;

/// Fetch PR data from the daemon via the `prs.status` RPC.
///
/// Returns `None` if the daemon is not available, allowing fallback to direct gh CLI.
/// Coworker data is fetched separately via `fetch_coworker_status_via_rpc`.
fn fetch_kanban_data_via_rpc() -> KanbanRpcResult {
    use crate::client::DaemonClient;

    let client = DaemonClient::connect().ok()?;
    let data = client.prs_status().ok()?;

    let prs_json = data.get("prs").and_then(|v| v.as_array())?;
    let merged_json = data.get("merged_prs").and_then(|v| v.as_array())?;

    // Extract repo metadata if present
    let repos: Vec<(String, String)> = data
        .get("repos")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    let label = r.get("label").and_then(|v| v.as_str())?.to_string();
                    let full_name = r
                        .get("full_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some((label, full_name))
                })
                .collect()
        })
        .unwrap_or_default();

    let prs: Vec<KanbanPr> = prs_json
        .iter()
        .filter_map(|pr| {
            let number = pr.get("number").and_then(|v| v.as_u64())?;
            let title = pr
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let author = pr
                .get("author")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let created_at = pr
                .get("created_at")
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(Utc::now);
            let ci_status = match pr
                .get("ci_status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
            {
                "passed" => CiStatus::Passed,
                "failed" => CiStatus::Failed,
                "running" => CiStatus::Running,
                _ => CiStatus::Unknown,
            };
            let reviewer = pr
                .get("reviewer")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let reviewed_at = pr
                .get("reviewed_at")
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc));

            let review_posted = pr
                .get("review_posted")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let repo = pr
                .get("repo")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let task_id = pr.get("task_id").and_then(|v| v.as_u64());
            let task_name = pr
                .get("task_name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let has_conflicts = pr
                .get("has_conflicts")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            Some(KanbanPr {
                number,
                title,
                author,
                created_at,
                ci_status,
                reviewer,
                reviewed_at,
                review_posted,
                repo,
                task_id,
                task_name,
                has_conflicts,
            })
        })
        .collect();

    let merged_prs: Vec<MergedPr> = merged_json
        .iter()
        .filter_map(|pr| {
            let number = pr.get("number").and_then(|v| v.as_u64())?;
            let title = pr
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let merged_at = pr
                .get("merged_at")
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(Utc::now);
            let repo = pr
                .get("repo")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            Some(MergedPr {
                number,
                title,
                merged_at,
                repo,
            })
        })
        .collect();

    Some((prs, merged_prs, repos))
}

/// Fetch live coworker status from the daemon via the `coworkers.status` RPC.
///
/// Also fetches pending questions via `coworker.questions`. Returns `None` if
/// the daemon is unreachable so the caller can use a default empty value.
fn fetch_coworker_status_via_rpc() -> Option<CoworkerStatusData> {
    use crate::client::DaemonClient;

    let client = DaemonClient::connect().ok()?;
    let data = client.coworkers_status().ok()?;

    let coworkers_json = data.get("coworkers").and_then(|v| v.as_array());

    let max_coworkers = data
        .get("max_coworkers")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(10);

    let coworkers: Vec<CoworkerInfo> = coworkers_json
        .map(|arr| {
            arr.iter()
                .filter_map(|cw| {
                    let name = cw.get("name").and_then(|v| v.as_str())?.to_string();
                    let task_id = cw
                        .get("task_id")
                        .and_then(|v| v.as_u64())
                        .map(|id| id as u32);
                    let phase = cw
                        .get("phase")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let pr_number = cw.get("pr_number").and_then(|v| v.as_u64());
                    let health = cw
                        .get("health")
                        .and_then(|v| v.as_str())
                        .unwrap_or("green")
                        .to_string();
                    let provider = cw
                        .get("provider")
                        .and_then(|v| v.as_str())
                        .unwrap_or("claude")
                        .to_string();
                    let profile = cw
                        .get("profile")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(midtown::auth::current_profile);
                    let progress = cw.get("progress").and_then(|v| v.as_u64()).map(|p| p as u8);
                    let time_estimate = cw
                        .get("time_estimate")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    Some(CoworkerInfo {
                        name,
                        task_id,
                        phase,
                        pr_number,
                        health,
                        provider,
                        profile,
                        progress,
                        time_estimate,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let lead_working = data
        .get("lead_working")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let tool_activity: HashMap<String, Vec<String>> = data
        .get("tool_activity")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .map(|(agent, items)| {
                    let headers: Vec<String> = items
                        .as_array()
                        .map(|arr| extract_tool_activity_headers(arr))
                        .unwrap_or_default();
                    (agent.clone(), headers)
                })
                .collect()
        })
        .unwrap_or_default();

    let channel_lead_names: Vec<String> = data
        .get("channel_leads")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let pending_questions = fetch_pending_questions_via_rpc();

    Some(CoworkerStatusData {
        coworkers,
        max_coworkers,
        lead_working,
        tool_activity,
        pending_questions,
        channel_lead_names,
    })
}

/// Fetch pending questions from coworkers via daemon RPC.
///
/// Returns a (possibly empty) list of questions waiting for user input.
/// Silently returns empty on connection failure so the TUI degrades gracefully.
fn fetch_pending_questions_via_rpc() -> Vec<PendingQuestion> {
    use crate::client::DaemonClient;

    let client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let data = match client.coworker_questions() {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    data.get("questions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|q| {
                    let id = q.get("id").and_then(|v| v.as_u64())?;
                    let coworker_name =
                        q.get("coworker_name").and_then(|v| v.as_str())?.to_string();
                    let question = q.get("question").and_then(|v| v.as_str())?.to_string();
                    let timestamp = q
                        .get("timestamp")
                        .and_then(|v| v.as_str())
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                        .unwrap_or_else(chrono::Utc::now);
                    Some(PendingQuestion {
                        id,
                        coworker_name,
                        question,
                        timestamp,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Convert a list of serialized `UniversalItem`s into display strings.
///
/// ToolResult items are folded into their matching ToolCall: when a ToolResult
/// exists for a call, the ToolCall header gains a `✓` (success) or `✗` (error)
/// prefix. In-progress calls (no result yet) use `›`. ToolResult items are
/// not emitted as standalone entries.
fn extract_tool_activity_headers(items: &[serde_json::Value]) -> Vec<String> {
    use std::collections::HashMap;

    // First pass: collect result status keyed by call_id.
    let mut result_status: HashMap<&str, bool> = HashMap::new();
    for item in items {
        if let Some(content) = item.get("content").and_then(|v| v.as_array()) {
            for part in content {
                if let Some(result) = part.get("ToolResult")
                    && let Some(call_id) = result.get("call_id").and_then(|v| v.as_str())
                {
                    let is_error = result
                        .get("is_error")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    result_status.insert(call_id, is_error);
                }
            }
        }
    }

    // Second pass: emit one display string per ToolCall, annotated with result status.
    let mut headers = Vec::new();
    for item in items {
        if let Some(content) = item.get("content").and_then(|v| v.as_array()) {
            for part in content {
                if let Some(call) = part.get("ToolCall") {
                    let header = call
                        .get("semantic_header")
                        .and_then(|v| v.as_str())
                        .unwrap_or_else(|| {
                            call.get("name").and_then(|v| v.as_str()).unwrap_or("?")
                        });
                    let call_id = call.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
                    let display = match result_status.get(call_id) {
                        Some(false) => format!("\u{2713} {header}"), // ✓ header
                        Some(true) => format!("\u{2717} {header}"),  // ✗ header
                        None => format!("\u{203a} {header}"),        // › header (in-progress)
                    };
                    headers.push(display);
                    break; // one ToolCall per item
                }
            }
        }
    }
    headers
}

/// Merge incoming tool activity headers with existing entries, preserving completed_at timestamps.
///
/// When an entry transitions from in-progress (›) to completed (✓/✗), records the current
/// instant as `completed_at`. Completed entries that were already tracked preserve their
/// original `completed_at` timestamp so age-out logic can measure elapsed time correctly.
///
/// Matching between old and new entries is done by comparing the body text (everything after
/// the first character prefix and leading whitespace), allowing a "› Read foo.rs" to match
/// a "✓ Read foo.rs" across ticks.
fn merge_tool_activity(
    old: HashMap<String, Vec<ToolActivityEntry>>,
    new: HashMap<String, Vec<String>>,
) -> HashMap<String, Vec<ToolActivityEntry>> {
    new.into_iter()
        .map(|(agent, headers)| {
            let old_entries = old.get(&agent);
            let entries = headers
                .into_iter()
                .map(|header| {
                    let is_completed = header.starts_with('\u{2713}') // ✓
                        || header.starts_with('\u{2717}'); // ✗
                    let completed_at = if is_completed {
                        // Extract body text: everything after the prefix char and leading space.
                        let body: &str = header[header
                            .char_indices()
                            .nth(1)
                            .map(|(i, _)| i)
                            .unwrap_or(header.len())..]
                            .trim_start();
                        // Look for a matching old entry by body text to preserve its timestamp.
                        old_entries
                            .and_then(|entries| {
                                entries.iter().find(|e| {
                                    let old_body = e.header[e
                                        .header
                                        .char_indices()
                                        .nth(1)
                                        .map(|(i, _)| i)
                                        .unwrap_or(e.header.len())..]
                                        .trim_start();
                                    old_body == body
                                })
                            })
                            .and_then(|e| e.completed_at)
                            .or_else(|| Some(std::time::Instant::now()))
                    } else {
                        None
                    };
                    ToolActivityEntry {
                        header,
                        completed_at,
                    }
                })
                .collect();
            (agent, entries)
        })
        .collect()
}

/// Cache for default branch names, keyed by repo full name (or empty string for current repo).
/// Avoids an API call on every repo status refresh since the default branch rarely changes.
static DEFAULT_BRANCH_CACHE: Mutex<Option<Vec<(String, String)>>> = Mutex::new(None);

/// Look up cached default branch for the given repo key.
fn cached_default_branch(key: &str) -> Option<String> {
    DEFAULT_BRANCH_CACHE
        .lock()
        .ok()?
        .as_ref()?
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
}

/// Store a default branch in the cache.
fn cache_default_branch(key: &str, branch: &str) {
    if let Ok(mut guard) = DEFAULT_BRANCH_CACHE.lock() {
        let entries = guard.get_or_insert_with(Vec::new);
        if let Some(entry) = entries.iter_mut().find(|(k, _)| k == key) {
            entry.1 = branch.to_string();
        } else {
            entries.push((key.to_string(), branch.to_string()));
        }
    }
}

/// Detect the default branch for a repository.
///
/// Uses a process-lifetime cache to avoid repeated API calls, since the
/// default branch rarely changes. Falls back to "main" on API failure.
fn detect_default_branch_for_repo(repo_full_name: Option<&str>) -> String {
    let cache_key = repo_full_name.unwrap_or("");
    if let Some(cached) = cached_default_branch(cache_key) {
        return cached;
    }
    let api_path = match repo_full_name {
        Some(name) => format!("repos/{}", name),
        None => "repos/{owner}/{repo}".to_string(),
    };
    if let Ok(output) = std::process::Command::new("gh")
        .args(["api", &api_path, "--jq", ".default_branch"])
        .output()
        && output.status.success()
    {
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !branch.is_empty() {
            cache_default_branch(cache_key, &branch);
            return branch;
        }
    }
    let fallback = "main".to_string();
    cache_default_branch(cache_key, &fallback);
    fallback
}

/// Fetch repository status (commit, CI status, release) from GitHub using gh CLI.
///
/// If `repo_full_name` is provided (e.g., "btucker/midtown"), uses explicit API paths.
/// Otherwise, uses gh template variables that resolve from the current git context.
fn fetch_repo_status(repo_full_name: Option<&str>) -> RepoStatus {
    let mut status = RepoStatus::default();

    // Detect the default branch for CI status queries
    let default_branch = detect_default_branch_for_repo(repo_full_name);

    // Build API path prefix: explicit repo or gh template variable
    let (commits_path, actions_path, releases_path) = match repo_full_name {
        Some(name) => (
            format!("repos/{}/commits/HEAD", name),
            format!(
                "repos/{}/actions/runs?branch={}&per_page=1",
                name, default_branch
            ),
            format!("repos/{}/releases/latest", name),
        ),
        None => (
            "repos/{owner}/{repo}/commits/{branch}".to_string(),
            format!(
                "repos/{{owner}}/{{repo}}/actions/runs?branch={}&per_page=1",
                default_branch
            ),
            "repos/{owner}/{repo}/releases/latest".to_string(),
        ),
    };

    // Fetch latest commit on default branch
    if let Ok(output) = std::process::Command::new("gh")
        .args([
            "api",
            &commits_path,
            "--jq",
            r#"{sha: .sha[0:7], date: .commit.author.date}"#,
        ])
        .output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(&stdout) {
            if let Some(sha) = data.get("sha").and_then(|v| v.as_str()) {
                status.commit_hash = sha.to_string();
            }
            if let Some(date_str) = data.get("date").and_then(|v| v.as_str()) {
                status.commit_time = DateTime::parse_from_rfc3339(date_str)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc));
            }
        }
    }

    // Fetch CI status from latest workflow run on main branch
    if let Ok(output) = std::process::Command::new("gh")
        .args([
            "api",
            &actions_path,
            "--jq",
            ".workflow_runs[0] | {status: .status, conclusion: .conclusion}",
        ])
        .output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(&stdout) {
            let run_status = data.get("status").and_then(|v| v.as_str()).unwrap_or("");
            let conclusion = data.get("conclusion").and_then(|v| v.as_str());

            status.ci_status = match (run_status, conclusion) {
                ("completed", Some("success")) => CiStatus::Passed,
                ("completed", Some("failure")) => CiStatus::Failed,
                ("completed", Some("cancelled")) => CiStatus::Failed,
                ("in_progress", _) | ("queued", _) | ("waiting", _) => CiStatus::Running,
                _ => CiStatus::Unknown,
            };
        }
    }

    // Fetch latest release
    if let Ok(output) = std::process::Command::new("gh")
        .args([
            "api",
            &releases_path,
            "--jq",
            "{tag: .tag_name, published_at: .published_at}",
        ])
        .output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(&stdout) {
            if let Some(tag) = data.get("tag").and_then(|v| v.as_str()) {
                status.release_tag = Some(tag.to_string());
            }
            if let Some(date_str) = data.get("published_at").and_then(|v| v.as_str()) {
                status.release_time = DateTime::parse_from_rfc3339(date_str)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc));
            }
        }
    }

    status
}

#[path = "autocomplete_tests.rs"]
#[cfg(test)]
mod autocomplete_tests;

#[path = "spinner_tests.rs"]
#[cfg(test)]
mod spinner_tests;

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use midtown::Message;
    use std::thread;
    use std::time::Duration;

    /// Helper to retry Channel operations that may fail with WouldBlock due to lock contention.
    /// This mirrors the retry_with_backoff helper in channel.rs tests.
    fn retry_with_backoff<T>(
        max_attempts: u32,
        mut f: impl FnMut() -> midtown::Result<T>,
    ) -> midtown::Result<T> {
        if max_attempts == 0 {
            return Err(midtown::Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "retry_with_backoff: max_attempts must be > 0",
            )));
        }
        for attempt in 0..max_attempts {
            match f() {
                Ok(val) => return Ok(val),
                Err(e) if attempt < max_attempts - 1 => {
                    thread::sleep(Duration::from_millis(10 * (attempt as u64 + 1)));
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        unreachable!("loop above always returns")
    }

    #[test]
    fn test_retry_with_backoff_zero_attempts_returns_error() {
        let result = retry_with_backoff(0, || Ok(42));
        assert!(result.is_err(), "max_attempts=0 should return an error");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("max_attempts must be > 0"),
            "Error should mention max_attempts, got: {err_msg}"
        );
    }

    /// Create a default App for testing. Tests can override specific fields
    /// using struct update syntax: `App { messages, ..test_app() }`
    pub(crate) fn test_app() -> App {
        App {
            messages: VecDeque::new(),
            scroll_offset: 0,
            visible_height: 20,
            channel: None,
            initial_load_done: true,
            history_start_position: 0,
            history_fully_loaded: true,
            test_mode: true, // Prevent daemon communication in tests
            #[cfg(test)]
            last_posted_channel: None,
            #[cfg(test)]
            load_channel_messages_called: false,
            tasks: Vec::new(),
            prs: Vec::new(),
            merged_prs: Vec::new(),
            coworkers: Vec::new(),
            lead_working: false,
            tool_activity: HashMap::new(),
            channel_lead_thinking: HashMap::new(),
            max_coworkers: 10, // Test default
            pending_questions: Vec::new(),
            repo_name: "test".to_string(),
            kanban_last_refresh: Instant::now(),
            kanban_receiver: None,
            coworker_status_last_refresh: Instant::now(),
            coworker_status_receiver: None,
            repo_status: RepoStatus::default(),
            repo_statuses: Vec::new(),
            repo_status_last_refresh: Instant::now(),
            repo_status_receiver: None,
            user_display_name: None,
            current_tasks_cache: HashMap::new(),
            tasks_cache_hash: 0,
            intentionally_at_top: false,
            mouse_scroll_accumulator: 0,
            mermaid_cache: MermaidCache::new(),
            diagram_sources: Vec::new(),
            usage_data: Vec::new(),
            usage_receiver: None,
            usage_last_refresh: Instant::now(),
            focused_pane: FocusedPane::Board,
            board_selection: None,
            selected_channel: "midtown".to_string(),
            selected_channel_archived: false,
            input_text: String::new(),
            input_cursor: 0,
            pending_image: None,
            selection_mode: false,
            message_render_cache: None,
            channel_unread_counts: HashMap::new(),
            autocomplete: AutocompleteState::default(),
            channel_switcher: ChannelSwitcherState::default(),
            show_archived_channels: false,
            spinner_frame: 0,
            spinner_last_tick: Instant::now(),
            channel_lead_names: Vec::new(),
            available_channels: Vec::new(),
            channels_last_refresh: Instant::now(),
            project_name: "test".to_string(),
            board_area: None,
            chat_messages_area: None,
            input_area: None,
            thread_input_area: None,
            task_line_map: HashMap::new(),
            channel_line_map: HashMap::new(),
            thread_reply_line_map: HashMap::new(),
            sidebar_width_pct: 40,
            divider_x: None,
            dragging_divider: false,
            layout_width: 0,
            main_area_y: 0,
            main_area_bottom: u16::MAX,
            kill_ring: None,
            last_was_kill: false,
            thread_parent_id: None,
            thread_messages: Vec::new(),
            thread_input_text: String::new(),
            thread_input_cursor: 0,
            ops_messages: VecDeque::new(),
            ops_channel: None,
            ops_initial_load_done: true,
        }
    }

    #[test]
    fn test_resize_sidebar_to_normal() {
        let mut app = test_app();
        app.resize_sidebar_to(40, 100);
        assert_eq!(app.sidebar_width_pct, 40);
    }

    #[test]
    fn test_resize_sidebar_to_clamps_min() {
        let mut app = test_app();
        app.resize_sidebar_to(5, 100); // 5% → clamped to 20%
        assert_eq!(app.sidebar_width_pct, 20);
    }

    #[test]
    fn test_resize_sidebar_to_clamps_max() {
        let mut app = test_app();
        app.resize_sidebar_to(80, 100); // 80% → clamped to 60%
        assert_eq!(app.sidebar_width_pct, 60);
    }

    #[test]
    fn test_resize_sidebar_to_zero_width_noop() {
        let mut app = test_app();
        app.sidebar_width_pct = 40;
        app.resize_sidebar_to(50, 0); // terminal_width=0 → no-op
        assert_eq!(app.sidebar_width_pct, 40);
    }

    #[test]
    fn test_resize_sidebar_invalidates_render_cache() {
        use ratatui::text::Line;
        let mut app = test_app();
        app.message_render_cache = Some(super::MessageRenderCache::new(
            vec![Line::raw("cached")],
            vec![],
            42,
        ));
        app.resize_sidebar_to(50, 100);
        assert!(
            app.message_render_cache.is_none(),
            "resize_sidebar_to should invalidate the message render cache"
        );
    }

    #[test]
    fn test_message_cache_key_changes_with_height() {
        let app = test_app();
        let key_short = app.message_cache_key(80, 20);
        let key_tall = app.message_cache_key(80, 30);
        assert_ne!(
            key_short, key_tall,
            "message cache key should include chat height"
        );
    }

    #[test]
    fn test_mouse_scroll_accumulator() {
        // Test that mouse wheel scrolling requires multiple events per line
        // for smooth scrolling (reduces scroll speed compared to keyboard).
        // Each 8 mouse events triggers one scroll_up/down which moves by SCROLL_STEP.
        let mut app = test_app();

        // Add enough messages to make scrolling possible
        // visible_height = 20, so we need > 20 messages
        for i in 0..30 {
            app.messages
                .push_back(Message::text("test", format!("Test message {}", i)));
        }

        // Start at the bottom (scroll_offset = 0)
        app.scroll_offset = 0;

        // Test scroll up: should require 8 events to scroll SCROLL_STEP lines
        let initial_offset = app.scroll_offset;

        // First 7 events should not scroll
        for _ in 0..7 {
            app.mouse_scroll_up();
        }
        assert_eq!(
            app.scroll_offset, initial_offset,
            "Should not scroll with <8 events"
        );

        // 8th event should trigger scroll by SCROLL_STEP
        app.mouse_scroll_up();
        assert_eq!(
            app.scroll_offset,
            initial_offset + SCROLL_STEP,
            "Should scroll after 8 events"
        );

        // Accumulator should reset, so another 8 events needed
        for _ in 0..7 {
            app.mouse_scroll_up();
        }
        assert_eq!(
            app.scroll_offset,
            initial_offset + SCROLL_STEP,
            "Should not scroll with <8 events after reset"
        );

        app.mouse_scroll_up();
        assert_eq!(
            app.scroll_offset,
            initial_offset + SCROLL_STEP * 2,
            "Should scroll after another 8 events"
        );

        // Test scroll down
        let current_offset = app.scroll_offset;
        for _ in 0..8 {
            app.mouse_scroll_down();
        }
        assert_eq!(
            app.scroll_offset,
            current_offset - SCROLL_STEP,
            "Scroll down should work after 8 events"
        );
    }

    #[test]
    fn test_task_status_from_string() {
        // Test the status parsing logic
        let status_str = "in_progress";
        let status = match status_str {
            "in_progress" => TaskStatus::InProgress,
            "completed" => TaskStatus::Completed,
            _ => TaskStatus::Pending,
        };
        assert_eq!(status, TaskStatus::InProgress);

        let status_str = "completed";
        let status = match status_str {
            "in_progress" => TaskStatus::InProgress,
            "completed" => TaskStatus::Completed,
            _ => TaskStatus::Pending,
        };
        assert_eq!(status, TaskStatus::Completed);

        let status_str = "pending";
        let status = match status_str {
            "in_progress" => TaskStatus::InProgress,
            "completed" => TaskStatus::Completed,
            _ => TaskStatus::Pending,
        };
        assert_eq!(status, TaskStatus::Pending);
    }

    #[test]
    fn test_kanban_task_clone() {
        let task = KanbanTask {
            id: "1".to_string(),
            subject: "Test task".to_string(),
            owner: Some("park".to_string()),
            status: TaskStatus::InProgress,
            modified_at: None,
            channel: None,
            blocked_by: vec![],
        };
        let cloned = task.clone();
        assert_eq!(cloned.id, "1");
        assert_eq!(cloned.subject, "Test task");
        assert_eq!(cloned.owner, Some("park".to_string()));
        assert_eq!(cloned.status, TaskStatus::InProgress);
    }

    #[test]
    fn test_tasks_by_status_groups_correctly() {
        let app = App {
            tasks: vec![
                KanbanTask {
                    id: "1".to_string(),
                    subject: "Pending task".to_string(),
                    owner: None,
                    status: TaskStatus::Pending,
                    modified_at: None,
                    channel: None,
                    blocked_by: vec![],
                },
                KanbanTask {
                    id: "2".to_string(),
                    subject: "In progress task".to_string(),
                    owner: Some("park".to_string()),
                    status: TaskStatus::InProgress,
                    modified_at: None,
                    channel: None,
                    blocked_by: vec![],
                },
                KanbanTask {
                    id: "3".to_string(),
                    subject: "Completed task".to_string(),
                    owner: Some("lexington".to_string()),
                    status: TaskStatus::Completed,
                    modified_at: None,
                    channel: None,
                    blocked_by: vec![],
                },
            ],
            ..test_app()
        };

        let (pending, in_progress, completed) = app.tasks_by_status();
        assert_eq!(pending.len(), 1);
        assert_eq!(in_progress.len(), 1);
        assert_eq!(completed.len(), 1);
        assert_eq!(pending[0].id, "1");
        assert_eq!(in_progress[0].id, "2");
        assert_eq!(completed[0].id, "3");
    }

    #[test]
    fn test_initial_load_shows_most_recent_messages() {
        // Simulate app state after loading 50 messages with visible_height of 10
        // scroll_offset=0 should mean we see the LAST 10 messages (most recent)
        let messages: VecDeque<Message> = (0..50)
            .map(|i| Message {
                id: i.to_string(),
                from: "test".to_string(),
                content: format!("message {}", i),
                timestamp: chrono::Utc::now(),
                message_type: midtown::MessageType::Text,
                channel: None,
                source_channel: None,
                session_id: None,
                thread_parent_id: None,
            })
            .collect();

        let mut app = App {
            messages,
            visible_height: 10,
            ..test_app()
        };

        let visible = app.visible_messages();
        assert_eq!(visible.len(), 10);
        // Should show messages 40-49 (the last 10)
        assert_eq!(visible[0].id, "40");
        assert_eq!(visible[9].id, "49");
    }

    #[test]
    fn test_parse_task_json() {
        let json = r#"{
            "id": "3",
            "subject": "Fix Backlog & In Progress kanban columns",
            "description": "Test description",
            "owner": "park",
            "status": "in_progress",
            "blocks": [],
            "blockedBy": []
        }"#;

        let task_data: serde_json::Value = serde_json::from_str(json).unwrap();

        let id = task_data.get("id").and_then(|v| v.as_str()).unwrap();
        let subject = task_data.get("subject").and_then(|v| v.as_str()).unwrap();
        let owner = task_data.get("owner").and_then(|v| v.as_str());
        let status_str = task_data.get("status").and_then(|v| v.as_str()).unwrap();

        assert_eq!(id, "3");
        assert_eq!(subject, "Fix Backlog & In Progress kanban columns");
        assert_eq!(owner, Some("park"));
        assert_eq!(status_str, "in_progress");
    }

    #[test]
    fn test_extract_coworker_from_body() {
        // With coworker frontmatter
        let body = "<!-- midtown: york -->\n\n## Summary\n- Added feature";
        assert_eq!(extract_coworker_from_body(body), Some("york".to_string()));

        // With extra whitespace
        let body = "<!--midtown:  park  -->\n\nDescription";
        assert_eq!(extract_coworker_from_body(body), Some("park".to_string()));

        // No frontmatter
        let body = "## Summary\nJust a regular PR";
        assert_eq!(extract_coworker_from_body(body), None);

        // Empty body
        assert_eq!(extract_coworker_from_body(""), None);

        // Malformed frontmatter (no closing)
        let body = "<!-- midtown: york\n## Summary";
        assert_eq!(extract_coworker_from_body(body), None);
    }

    #[test]
    fn test_extract_reviewer_from_header() {
        assert_eq!(
            extract_reviewer_from_header("## Code Review by york\nNo issues found."),
            Some("york".to_string())
        );
        assert_eq!(
            extract_reviewer_from_header("### Code review by madison\nLooks good."),
            Some("madison".to_string())
        );
        assert_eq!(
            extract_reviewer_from_header("Code Review by park"),
            Some("park".to_string())
        );
        // No reviewer header
        assert_eq!(extract_reviewer_from_header("Just a regular comment"), None);
        assert_eq!(extract_reviewer_from_header(""), None);
    }

    #[test]
    fn test_extract_reviewer_from_comments() {
        // Comment with midtown frontmatter
        let comments = vec![serde_json::json!({
            "body": "<!-- midtown: lexington -->\n\n### Code review\n\nNo issues found.",
            "createdAt": "2026-01-29T10:00:00Z"
        })];
        let (reviewer, at) = extract_reviewer_from_comments(&comments);
        assert_eq!(reviewer, Some("lexington".to_string()));
        assert!(at.is_some());

        // Comment with "Code Review by" header
        let comments = vec![serde_json::json!({
            "body": "## Code Review by vernon\nLGTM",
            "createdAt": "2026-01-29T11:00:00Z"
        })];
        let (reviewer, at) = extract_reviewer_from_comments(&comments);
        assert_eq!(reviewer, Some("vernon".to_string()));
        assert!(at.is_some());

        // No review comment
        let comments = vec![serde_json::json!({
            "body": "This is a regular comment",
            "createdAt": "2026-01-29T12:00:00Z"
        })];
        let (reviewer, _) = extract_reviewer_from_comments(&comments);
        assert_eq!(reviewer, None);

        // Empty comments
        let (reviewer, _) = extract_reviewer_from_comments(&[]);
        assert_eq!(reviewer, None);
    }

    #[test]
    fn test_is_at_max_scroll() {
        // Create an App with 100 messages and visible_height of 20
        let messages: VecDeque<Message> = (0..100)
            .map(|i| Message {
                id: i.to_string(),
                from: "test".to_string(),
                content: format!("message {}", i),
                timestamp: chrono::Utc::now(),
                message_type: midtown::MessageType::Text,
                channel: None,
                source_channel: None,
                session_id: None,
                thread_parent_id: None,
            })
            .collect();

        let mut app = App {
            messages,
            ..test_app()
        };

        // At bottom (scroll_offset=0): not at max scroll
        assert!(
            !app.is_at_max_scroll(),
            "scroll_offset=0 should not be at max"
        );

        // Somewhere in the middle: not at max scroll
        app.scroll_offset = 40;
        assert!(
            !app.is_at_max_scroll(),
            "scroll_offset=40 should not be at max"
        );

        // At max scroll position but NOT intentionally - should NOT be considered at max
        // This is the key behavior change: scroll_offset >= max is not enough,
        // we must have intentionally scrolled there
        app.scroll_offset = 80;
        assert!(
            !app.is_at_max_scroll(),
            "scroll_offset=80 without intentionally_at_top should NOT be at max"
        );

        // Use scroll_to_top() to INTENTIONALLY scroll to max
        app.scroll_to_top();
        assert!(
            app.is_at_max_scroll(),
            "After scroll_to_top(), should be at max"
        );

        // Beyond max scroll with intentional flag: should still be considered at max
        app.scroll_offset = 85;
        app.intentionally_at_top = true;
        assert!(
            app.is_at_max_scroll(),
            "scroll_offset=85 (beyond max) with intentional flag should be at max"
        );

        // Scrolling down clears the intentional flag
        app.scroll_down();
        assert!(
            !app.is_at_max_scroll(),
            "After scroll_down, should no longer be at max"
        );
    }

    #[test]
    fn test_visible_height_increase_should_not_trigger_max_scroll() {
        // BUG REPRODUCTION: When visible_height increases (e.g., kanban shrinks),
        // a previously mid-scroll position can suddenly become "at max scroll",
        // causing the display to jump to showing oldest messages.
        //
        // Scenario:
        // - 100 messages, visible_height=20, user scrolled to offset=50 (middle)
        // - max_scroll = 100 - 20 = 80, so scroll_offset=50 < 80, NOT at max
        // - Kanban shrinks, visible_height increases to 60
        // - max_scroll = 100 - 60 = 40, now scroll_offset=50 >= 40, AT MAX!
        // - Display suddenly shows oldest messages instead of middle

        let messages: VecDeque<Message> = (0..100)
            .map(|i| Message {
                id: i.to_string(),
                from: "test".to_string(),
                content: format!("message {}", i),
                timestamp: chrono::Utc::now(),
                message_type: midtown::MessageType::Text,
                channel: None,
                source_channel: None,
                session_id: None,
                thread_parent_id: None,
            })
            .collect();

        let mut app = App {
            messages,
            scroll_offset: 50, // User scrolled to middle
            ..test_app()
        };

        // Verify initial state: NOT at max scroll
        assert!(
            !app.is_at_max_scroll(),
            "scroll_offset=50 with visible_height=20 should NOT be at max (max=80)"
        );

        // Simulate kanban shrinking: visible_height increases from 20 to 60
        app.visible_height = 60;

        // BUG: After this change, is_at_max_scroll would return TRUE because
        // scroll_offset=50 >= max_scroll=40. This causes the display to jump
        // to showing oldest messages.
        //
        // FIX: After updating visible_height, clamp scroll_offset to stay within
        // valid bounds, preventing unexpected "at max scroll" state.
        app.clamp_scroll_offset();

        // After the fix, we should NOT be at max scroll
        // (scroll_offset should be clamped to max, and is_at_max_scroll should
        // only return true when we're intentionally viewing the oldest messages)
        assert!(
            !app.is_at_max_scroll(),
            "After visible_height increase and clamping, should NOT suddenly be at max scroll"
        );

        // The scroll_offset should be clamped to max_scroll
        assert_eq!(
            app.scroll_offset, 40,
            "scroll_offset should be clamped to max_scroll"
        );
    }

    #[test]
    fn test_incremental_scroll_up_to_top_sets_intentionally_at_top() {
        // Verify that scrolling up incrementally (not via scroll_to_top) still
        // sets intentionally_at_top when reaching max_scroll.
        let messages: VecDeque<Message> = (0..20)
            .map(|i| Message {
                id: i.to_string(),
                from: "test".to_string(),
                content: format!("message {}", i),
                timestamp: chrono::Utc::now(),
                message_type: midtown::MessageType::Text,
                channel: None,
                source_channel: None,
                session_id: None,
                thread_parent_id: None,
            })
            .collect();

        let mut app = App {
            messages,
            visible_height: 10,
            ..test_app()
        };

        // max_scroll = 20 - 10 = 10
        assert_eq!(app.max_scroll(), 10);
        assert!(!app.is_at_max_scroll(), "Should not be at max initially");

        // Scroll up incrementally until we reach max
        for _ in 0..10 {
            app.scroll_up();
        }

        // Should now be at max scroll
        assert_eq!(app.scroll_offset, 10);
        assert!(
            app.is_at_max_scroll(),
            "Incremental scroll_up to max should set intentionally_at_top"
        );

        // Reset and test page_up
        app.scroll_offset = 0;
        app.intentionally_at_top = false;
        assert!(!app.is_at_max_scroll());

        // Page up should also set the flag when reaching max
        app.page_up(); // page_size = 10 - 2 = 8, so scroll_offset = 8
        assert!(
            !app.is_at_max_scroll(),
            "First page_up should not reach max"
        );

        app.page_up(); // scroll_offset = min(8 + 8, 10) = 10
        assert!(
            app.is_at_max_scroll(),
            "page_up to max should set intentionally_at_top"
        );
    }

    #[test]
    fn test_mouse_wheel_scroll_is_slower_than_keyboard() {
        // Mouse wheel scrolling should be slower than keyboard scrolling for smoother UX.
        // Mouse wheels send multiple events per physical scroll, so we use fractional
        // scrolling: 8 wheel events = SCROLL_STEP lines of movement.
        // Keyboard scrolls SCROLL_STEP per call; mouse needs 8 events per SCROLL_STEP.
        let messages: VecDeque<Message> = (0..30)
            .map(|i| Message {
                id: i.to_string(),
                from: "user".to_string(),
                content: format!("message {}", i),
                timestamp: Utc::now(),
                message_type: midtown::MessageType::Text,
                channel: None,
                source_channel: None,
                session_id: None,
                thread_parent_id: None,
            })
            .collect();

        let mut app = App {
            messages,
            visible_height: 10,
            ..test_app()
        };

        // Start at bottom
        assert_eq!(app.scroll_offset, 0);

        // Keyboard scroll up: should move SCROLL_STEP lines per call
        app.scroll_up();
        assert_eq!(
            app.scroll_offset, SCROLL_STEP,
            "Keyboard scroll should move SCROLL_STEP lines"
        );

        // Reset
        app.scroll_offset = 0;

        // Mouse wheel scroll up: should take 8 events to move SCROLL_STEP lines
        for i in 1..=7 {
            app.mouse_scroll_up();
            assert_eq!(app.scroll_offset, 0, "Event {} shouldn't scroll yet", i);
        }

        app.mouse_scroll_up();
        assert_eq!(
            app.scroll_offset, SCROLL_STEP,
            "Eighth wheel event should complete SCROLL_STEP lines of scroll"
        );

        // Ninth event starts accumulating for next batch
        app.mouse_scroll_up();
        assert_eq!(
            app.scroll_offset, SCROLL_STEP,
            "Ninth wheel event accumulates"
        );

        // Test mouse wheel scroll down
        // Accumulator is at 1 from the ninth up event
        for i in 1..=6 {
            app.mouse_scroll_down();
            assert_eq!(
                app.scroll_offset,
                SCROLL_STEP,
                "Down event {} accumulates (acc={})",
                i,
                i + 1
            );
        }

        app.mouse_scroll_down();
        assert_eq!(
            app.scroll_offset, 0,
            "Seventh down event triggers scroll (acc=8)"
        );
    }

    #[test]
    fn test_channel_read_after_send_with_retry() {
        // This test reproduces the race condition that causes WouldBlock errors:
        // send() acquires a write lock, and if read_all() is called immediately after,
        // it may fail with WouldBlock because try_lock_shared() is non-blocking.
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path(), "test-channel").unwrap();

        // Add a message
        channel
            .send(&Message::text("alice", "First message"))
            .unwrap();

        // Immediate read after send can fail without retry
        // This should NOT panic even under lock contention
        let messages = retry_with_backoff(5, || channel.read_all()).unwrap();
        assert_eq!(messages.len(), 1);

        // Add another message and verify retry works
        channel
            .send(&Message::text("bob", "Second message"))
            .unwrap();

        let messages = retry_with_backoff(5, || channel.read_all()).unwrap();
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn test_unread_count_calculation() {
        use tempfile::TempDir;

        // Create a temporary channel with messages
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path(), "test-channel").unwrap();

        // Add some messages
        channel
            .send(&Message::text("alice", "First message"))
            .unwrap();
        channel
            .send(&Message::text("bob", "Second message"))
            .unwrap();
        channel
            .send(&Message::text("alice", "Third message"))
            .unwrap();

        // Create an app with this channel
        let mut app = App {
            channel: Some(channel.clone()),
            ..test_app()
        };

        // Small sleep to avoid WouldBlock from lock contention after send()
        // refresh_unread_counts() internally calls channel.read_all() which
        // uses try_lock_shared() and can fail if send() hasn't released the write lock yet
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Before any reading, all messages should be unread
        app.refresh_unread_counts();
        assert_eq!(
            app.channel_unread_counts.get("test-channel"),
            Some(&3),
            "All 3 messages should be unread initially"
        );

        // Simulate reading messages by updating the cursor
        let messages_read =
            retry_with_backoff(5, || channel.read_since_cursor("chat-tui")).unwrap();
        assert_eq!(messages_read.len(), 3, "Should have read 3 messages");

        // Verify cursor was saved - load it and check it points to the last message
        let cursor = channel.get_cursor("chat-tui").unwrap();
        assert!(
            cursor.last_message_id.is_some(),
            "Cursor should have last_message_id set after reading"
        );
        assert_eq!(
            cursor.last_message_id.as_ref(),
            Some(&messages_read[2].id),
            "Cursor should point to the last message read"
        );

        // Now refresh should show 0 unread (cursor is at EOF)
        app.refresh_unread_counts();
        assert_eq!(
            app.channel_unread_counts.get("test-channel"),
            None, // No entry means 0 unread (we filter out 0s)
            "After reading all messages, unread count should be 0"
        );

        // Add one more message
        channel
            .send(&Message::text("alice", "Fourth message"))
            .unwrap();

        // Verify we now have 4 total messages
        // Use retry to avoid WouldBlock from lock contention after send()
        let all_messages = retry_with_backoff(5, || channel.read_all()).unwrap();
        assert_eq!(all_messages.len(), 4, "Should have 4 total messages");

        // Verify cursor still points to message 3 (not auto-updated)
        let cursor_before_refresh = channel.get_cursor("chat-tui").unwrap();
        assert_eq!(
            cursor_before_refresh.last_message_id.as_ref(),
            Some(&messages_read[2].id),
            "Cursor should still point to message 3 before refresh"
        );

        // Small sleep to avoid WouldBlock from lock contention after send()
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Refresh should show 1 unread
        app.refresh_unread_counts();

        // Diagnostic: if test fails, show what we got
        let actual_count = app.channel_unread_counts.get("test-channel").copied();
        assert_eq!(
            actual_count,
            Some(1),
            "After new message arrives, unread count should be 1. \
             Cursor last_message_id: {:?}, All message IDs: {:?}",
            cursor_before_refresh.last_message_id,
            all_messages.iter().map(|m| &m.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_board_selection_navigation() {
        let mut app = App {
            tasks: vec![
                KanbanTask {
                    id: "1".to_string(),
                    subject: "Task 1".to_string(),
                    owner: None,
                    status: TaskStatus::Pending,
                    modified_at: None,
                    channel: Some("midtown".to_string()),
                    blocked_by: vec![],
                },
                KanbanTask {
                    id: "2".to_string(),
                    subject: "Task 2".to_string(),
                    owner: Some("park".to_string()),
                    status: TaskStatus::InProgress,
                    modified_at: None,
                    channel: Some("midtown".to_string()),
                    blocked_by: vec![],
                },
                KanbanTask {
                    id: "3".to_string(),
                    subject: "Task 3".to_string(),
                    owner: None,
                    status: TaskStatus::Pending,
                    modified_at: None,
                    channel: Some("features".to_string()),
                    blocked_by: vec![],
                },
            ],
            available_channels: vec![
                midtown::ChannelInfo {
                    name: "features".to_string(),
                    is_archived: false,
                },
                midtown::ChannelInfo {
                    name: "midtown".to_string(),
                    is_archived: false,
                },
            ],
            ..test_app()
        };

        // Initial state: no selection
        assert_eq!(app.board_selection, None);

        // Navigate down - should select first item (features channel)
        app.board_selection_down();
        assert!(
            matches!(
                &app.board_selection,
                Some(BoardSelection::Channel(ch)) if ch == "features"
            ),
            "First down should select first channel (features)"
        );

        // Navigate down again - should select first task under features channel
        app.board_selection_down();
        assert!(
            matches!(
                &app.board_selection,
                Some(BoardSelection::Task(ch, id)) if ch == "features" && id == "3"
            ),
            "Second down should select first task under features channel"
        );

        // Navigate down again - should select midtown channel
        app.board_selection_down();
        assert!(
            matches!(
                &app.board_selection,
                Some(BoardSelection::Channel(ch)) if ch == "midtown"
            ),
            "Third down should select midtown channel"
        );

        // Navigate up twice - should go back to first channel
        app.board_selection_up();
        app.board_selection_up();
        assert!(
            matches!(
                &app.board_selection,
                Some(BoardSelection::Channel(ch)) if ch == "features"
            ),
            "Up twice should go back to first channel"
        );
    }

    #[test]
    fn test_board_selection_changes_selected_channel() {
        let mut app = App {
            tasks: vec![
                KanbanTask {
                    id: "1".to_string(),
                    subject: "Task 1".to_string(),
                    owner: None,
                    status: TaskStatus::Pending,
                    modified_at: None,
                    channel: Some("midtown".to_string()),
                    blocked_by: vec![],
                },
                KanbanTask {
                    id: "2".to_string(),
                    subject: "Task 2".to_string(),
                    owner: None,
                    status: TaskStatus::Pending,
                    modified_at: None,
                    channel: Some("features".to_string()),
                    blocked_by: vec![],
                },
            ],
            ..test_app()
        };

        // Initial selected channel
        assert_eq!(app.selected_channel, "midtown");

        // Select a channel
        app.board_selection = Some(BoardSelection::Channel("features".to_string()));
        app.update_selected_channel();

        // Selected channel should update
        assert_eq!(app.selected_channel, "features");
    }

    #[test]
    fn test_valid_channel_names() {
        assert!(App::is_valid_channel_name("my-channel"));
        assert!(App::is_valid_channel_name("features"));
        assert!(App::is_valid_channel_name("task_42"));
        assert!(App::is_valid_channel_name("v2.0"));
        assert!(App::is_valid_channel_name("UPPER"));
        assert!(App::is_valid_channel_name("a"));
    }

    #[test]
    fn test_invalid_channel_names_path_traversal() {
        assert!(!App::is_valid_channel_name("../../etc/passwd"));
        assert!(!App::is_valid_channel_name(".."));
        assert!(!App::is_valid_channel_name("foo/bar"));
        assert!(!App::is_valid_channel_name("foo\\bar"));
        assert!(!App::is_valid_channel_name("../sibling"));
    }

    #[test]
    fn test_invalid_channel_names_special_chars() {
        assert!(!App::is_valid_channel_name(""));
        assert!(!App::is_valid_channel_name(".hidden"));
        assert!(!App::is_valid_channel_name("has space"));
        assert!(!App::is_valid_channel_name("has\0null"));
        assert!(!App::is_valid_channel_name("emoji🎉"));
        assert!(!App::is_valid_channel_name("semi;colon"));
    }

    // --- extract_tool_activity_headers tests ---

    fn tool_call_item(
        call_id: &str,
        name: &str,
        semantic_header: Option<&str>,
    ) -> serde_json::Value {
        let mut call = serde_json::json!({
            "call_id": call_id,
            "name": name,
        });
        if let Some(header) = semantic_header {
            call["semantic_header"] = serde_json::Value::String(header.to_string());
        }
        serde_json::json!({"content": [{"ToolCall": call}]})
    }

    fn tool_result_item(call_id: &str, is_error: bool) -> serde_json::Value {
        serde_json::json!({"content": [{"ToolResult": {"call_id": call_id, "is_error": is_error}}]})
    }

    #[test]
    fn test_extract_tool_activity_headers_empty() {
        let result = extract_tool_activity_headers(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_extract_tool_activity_headers_in_progress() {
        let items = vec![tool_call_item("id1", "Read", Some("Read foo.rs"))];
        let result = extract_tool_activity_headers(&items);
        assert_eq!(result, vec!["› Read foo.rs"]);
    }

    #[test]
    fn test_extract_tool_activity_headers_success() {
        let items = vec![
            tool_call_item("id1", "Read", Some("Read foo.rs")),
            tool_result_item("id1", false),
        ];
        let result = extract_tool_activity_headers(&items);
        assert_eq!(result, vec!["✓ Read foo.rs"]);
    }

    #[test]
    fn test_extract_tool_activity_headers_error() {
        let items = vec![
            tool_call_item("id1", "Bash", Some("Run tests")),
            tool_result_item("id1", true),
        ];
        let result = extract_tool_activity_headers(&items);
        assert_eq!(result, vec!["✗ Run tests"]);
    }

    #[test]
    fn test_extract_tool_activity_headers_falls_back_to_name() {
        // No semantic_header — should use "name" field
        let items = vec![tool_call_item("id1", "Read", None)];
        let result = extract_tool_activity_headers(&items);
        assert_eq!(result, vec!["› Read"]);
    }

    #[test]
    fn test_extract_tool_activity_headers_multiple_calls() {
        // Three calls: first completed, second errored, third in-progress
        let items = vec![
            tool_call_item("id1", "Read", Some("Read foo.rs")),
            tool_result_item("id1", false),
            tool_call_item("id2", "Bash", Some("Run tests")),
            tool_result_item("id2", true),
            tool_call_item("id3", "Write", Some("Write bar.rs")),
        ];
        let result = extract_tool_activity_headers(&items);
        assert_eq!(
            result,
            vec!["✓ Read foo.rs", "✗ Run tests", "› Write bar.rs"]
        );
    }

    #[test]
    fn test_extract_tool_activity_headers_result_before_call() {
        // Result appears before its matching ToolCall — should still annotate correctly
        // since first pass collects all results regardless of order.
        let items = vec![
            tool_result_item("id1", false),
            tool_call_item("id1", "Read", Some("Read foo.rs")),
        ];
        let result = extract_tool_activity_headers(&items);
        assert_eq!(result, vec!["✓ Read foo.rs"]);
    }

    #[test]
    fn test_extract_tool_activity_headers_skips_tool_result_items() {
        // ToolResult items should not appear as standalone entries in the output.
        let items = vec![
            tool_call_item("id1", "Read", Some("Read foo.rs")),
            tool_result_item("id1", false),
        ];
        let result = extract_tool_activity_headers(&items);
        assert_eq!(result.len(), 1, "Only one entry (the ToolCall)");
        assert_eq!(result[0], "✓ Read foo.rs");
    }

    // --- Thread state tests ---

    #[test]
    fn test_open_thread() {
        let mut app = test_app();
        // Add a parent message
        let parent = Message::text("agent1", "Hello");
        let parent_id = parent.id.clone();
        app.messages.push_back(parent);
        // Add a thread reply
        let mut reply = Message::text("agent2", "Reply");
        reply.thread_parent_id = Some(parent_id.clone());
        app.messages.push_back(reply);

        app.open_thread(&parent_id);
        assert_eq!(app.thread_parent_id, Some(parent_id.clone()));
        assert_eq!(app.thread_messages.len(), 1);
        assert_eq!(app.focused_pane, FocusedPane::Thread);
    }

    #[test]
    fn test_close_thread() {
        let mut app = test_app();
        app.thread_parent_id = Some("test-id".to_string());
        app.thread_messages = vec![Message::text("a", "b")];
        app.thread_input_text = "draft".to_string();
        app.focused_pane = FocusedPane::Thread;

        app.close_thread();
        assert!(app.thread_parent_id.is_none());
        assert!(app.thread_messages.is_empty());
        assert!(app.thread_input_text.is_empty());
        assert_eq!(app.focused_pane, FocusedPane::InputBar);
    }

    #[test]
    fn test_open_thread_nonexistent_id() {
        let mut app = test_app();
        app.open_thread("nonexistent");
        assert!(
            app.thread_parent_id.is_none(),
            "Should not open thread for nonexistent message ID"
        );
    }

    #[test]
    fn test_open_thread_collects_only_matching_replies() {
        let mut app = test_app();
        let parent = Message::text("agent1", "Parent");
        let parent_id = parent.id.clone();
        app.messages.push_back(parent);

        // Add a reply to this thread
        let mut reply1 = Message::text("agent2", "Thread reply");
        reply1.thread_parent_id = Some(parent_id.clone());
        app.messages.push_back(reply1);

        // Add a reply to a different thread
        let mut reply2 = Message::text("agent3", "Other thread reply");
        reply2.thread_parent_id = Some("other-parent-id".to_string());
        app.messages.push_back(reply2);

        // Add a top-level message
        app.messages.push_back(Message::text("agent4", "Top level"));

        app.open_thread(&parent_id);
        assert_eq!(
            app.thread_messages.len(),
            1,
            "Should only collect replies matching the open thread"
        );
        assert_eq!(app.thread_messages[0].content, "Thread reply");
    }

    #[test]
    fn test_open_thread_clears_previous_thread_input() {
        let mut app = test_app();
        let parent = Message::text("agent1", "Parent");
        let parent_id = parent.id.clone();
        app.messages.push_back(parent);

        // Simulate having an old draft
        app.thread_input_text = "old draft".to_string();
        app.thread_input_cursor = 5;

        app.open_thread(&parent_id);
        assert!(
            app.thread_input_text.is_empty(),
            "Opening a thread should clear input text"
        );
        assert_eq!(
            app.thread_input_cursor, 0,
            "Opening a thread should reset cursor"
        );
    }

    #[test]
    fn test_cycle_focus_with_thread() {
        let mut app = test_app();
        app.thread_parent_id = Some("test".to_string());
        app.focused_pane = FocusedPane::InputBar;
        app.cycle_focus();
        assert_eq!(
            app.focused_pane,
            FocusedPane::Thread,
            "Should cycle to Thread after InputBar when thread is open"
        );
        app.cycle_focus();
        assert_eq!(
            app.focused_pane,
            FocusedPane::Board,
            "Should cycle from Thread to Board"
        );
    }

    #[test]
    fn test_cycle_focus_without_thread() {
        let mut app = test_app();
        app.focused_pane = FocusedPane::InputBar;
        app.cycle_focus();
        assert_eq!(
            app.focused_pane,
            FocusedPane::Board,
            "Should skip Thread and go to Board when no thread is open"
        );
    }

    #[test]
    fn test_close_thread_resets_cursor() {
        let mut app = test_app();
        app.thread_parent_id = Some("test-id".to_string());
        app.thread_input_text = "some text".to_string();
        app.thread_input_cursor = 9;
        app.focused_pane = FocusedPane::Thread;

        app.close_thread();
        assert_eq!(app.thread_input_cursor, 0);
        assert!(app.thread_input_text.is_empty());
    }

    #[test]
    fn test_thread_autocomplete_trigger_detection() {
        let mut app = test_app();
        // Add some messages so autocomplete has items
        let msg1 = Message::text("park", "Hello world this is a test message");
        let msg2 = Message::text("madison", "Another top-level message here");
        app.messages.push_back(msg1);
        app.messages.push_back(msg2);

        // Type "/thread " — should trigger autocomplete with '/' trigger
        app.input_text = "/thread ".to_string();
        app.input_cursor = 8;
        app.detect_autocomplete_trigger();

        assert!(
            app.autocomplete.show,
            "Autocomplete should show after typing '/thread '"
        );
        assert_eq!(
            app.autocomplete.trigger_type,
            Some('/'),
            "Trigger type should be '/' for thread autocomplete"
        );
        assert!(
            !app.autocomplete.items.is_empty(),
            "Should have autocomplete items from messages"
        );
    }

    #[test]
    fn test_thread_autocomplete_filters_thread_replies() {
        let mut app = test_app();
        let msg1 = Message::text("park", "Top-level message");
        let parent_id = msg1.id.clone();
        let mut reply = Message::text("madison", "Thread reply");
        reply.thread_parent_id = Some(parent_id);
        app.messages.push_back(msg1);
        app.messages.push_back(reply);

        app.input_text = "/thread ".to_string();
        app.input_cursor = 8;
        app.detect_autocomplete_trigger();

        assert_eq!(
            app.autocomplete.items.len(),
            1,
            "Should only show top-level messages, not thread replies"
        );
        assert!(
            app.autocomplete.items[0]
                .description
                .as_ref()
                .unwrap()
                .contains("Top-level"),
            "Item should be the top-level message"
        );
    }

    #[test]
    fn test_thread_autocomplete_query_filters_messages() {
        let mut app = test_app();
        app.messages
            .push_back(Message::text("park", "Auth bug investigation"));
        app.messages
            .push_back(Message::text("madison", "Deploy pipeline fix"));

        // Type "/thread auth" — should filter to messages containing "auth"
        app.input_text = "/thread auth".to_string();
        app.input_cursor = 12;
        app.detect_autocomplete_trigger();

        assert!(
            app.autocomplete.show,
            "Autocomplete should show with query filter"
        );
        assert_eq!(
            app.autocomplete.items.len(),
            1,
            "Should only show messages matching 'auth'"
        );
    }

    #[test]
    fn test_thread_autocomplete_no_trigger_without_space() {
        let mut app = test_app();
        app.messages
            .push_back(Message::text("park", "Test message"));

        // "/thread" without trailing space should NOT trigger autocomplete
        app.input_text = "/thread".to_string();
        app.input_cursor = 7;
        app.detect_autocomplete_trigger();

        assert!(
            !app.autocomplete.show,
            "Autocomplete should not show for '/thread' without trailing space"
        );
    }

    #[test]
    fn test_thread_autocomplete_insert_opens_thread() {
        let mut app = test_app();
        let msg = Message::text("park", "Test parent message");
        let msg_id = msg.id.clone();
        app.messages.push_back(msg);

        // Set up autocomplete state as if user typed "/thread " and selected an item
        app.input_text = "/thread ".to_string();
        app.input_cursor = 8;
        app.autocomplete.show = true;
        app.autocomplete.trigger_type = Some('/');
        app.autocomplete.trigger_start_pos = 0;
        app.autocomplete.selected_index = 0;
        app.autocomplete.items = vec![AutocompleteItem {
            value: msg_id.clone(),
            description: Some("park: Test parent message".to_string()),
        }];

        app.insert_autocomplete_item();

        assert_eq!(
            app.thread_parent_id,
            Some(msg_id),
            "Selecting thread autocomplete item should open the thread"
        );
        assert!(
            app.input_text.is_empty(),
            "Input should be cleared after opening thread"
        );
        assert_eq!(app.input_cursor, 0, "Cursor should be reset");
        assert!(!app.autocomplete.show, "Autocomplete should be dismissed");
    }

    #[test]
    fn test_thread_autocomplete_shows_recent_first() {
        let mut app = test_app();
        // Add messages — most recent should appear first in autocomplete
        app.messages
            .push_back(Message::text("park", "First message"));
        app.messages
            .push_back(Message::text("madison", "Second message"));
        app.messages
            .push_back(Message::text("broadway", "Third message"));

        app.input_text = "/thread ".to_string();
        app.input_cursor = 8;
        app.detect_autocomplete_trigger();

        assert!(app.autocomplete.items.len() >= 3);
        // Most recent messages should appear first
        assert!(
            app.autocomplete.items[0]
                .description
                .as_ref()
                .unwrap()
                .contains("Third"),
            "Most recent message should be first"
        );
    }

    #[test]
    fn test_thread_autocomplete_limits_to_20() {
        let mut app = test_app();
        // Add 25 messages
        for i in 0..25 {
            app.messages
                .push_back(Message::text("park", format!("Message {}", i)));
        }

        app.input_text = "/thread ".to_string();
        app.input_cursor = 8;
        app.detect_autocomplete_trigger();

        assert!(
            app.autocomplete.items.len() <= 20,
            "Thread autocomplete should limit to 20 items, got {}",
            app.autocomplete.items.len()
        );
    }

    // --- merge_tool_activity tests ---

    #[test]
    fn test_merge_tool_activity_preserves_completed_at() {
        // Old: in-progress entry
        let old_entries = vec![ToolActivityEntry {
            header: "\u{203a} Read foo.rs".to_string(),
            completed_at: None,
        }];
        let old: HashMap<String, Vec<ToolActivityEntry>> =
            [("lead".to_string(), old_entries)].into_iter().collect();

        // New: same entry now completed
        let new: HashMap<String, Vec<String>> =
            [("lead".to_string(), vec!["\u{2713} Read foo.rs".to_string()])]
                .into_iter()
                .collect();

        let merged = merge_tool_activity(old, new);
        let entries = merged.get("lead").unwrap();
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].completed_at.is_some(),
            "Should have completed_at when transitioning from in-progress to completed"
        );
    }

    #[test]
    fn test_merge_tool_activity_preserves_existing_completed_at() {
        use std::time::{Duration, Instant};
        let old_time = Instant::now() - Duration::from_secs(10);
        let old_entries = vec![ToolActivityEntry {
            header: "\u{2713} Read foo.rs".to_string(),
            completed_at: Some(old_time),
        }];
        let old: HashMap<String, Vec<ToolActivityEntry>> =
            [("lead".to_string(), old_entries)].into_iter().collect();

        // New: same completed entry arrives again
        let new: HashMap<String, Vec<String>> =
            [("lead".to_string(), vec!["\u{2713} Read foo.rs".to_string()])]
                .into_iter()
                .collect();

        let merged = merge_tool_activity(old, new);
        let entries = merged.get("lead").unwrap();
        assert_eq!(entries.len(), 1);
        // completed_at should be preserved (same instant as old)
        assert!(
            entries[0].completed_at.unwrap().duration_since(old_time) < Duration::from_millis(1),
            "Should preserve old completed_at"
        );
    }

    #[test]
    fn test_merge_tool_activity_inprogress_has_no_timestamp() {
        let old: HashMap<String, Vec<ToolActivityEntry>> = HashMap::new();
        let new: HashMap<String, Vec<String>> = [(
            "lead".to_string(),
            vec!["\u{203a} Write bar.rs".to_string()],
        )]
        .into_iter()
        .collect();

        let merged = merge_tool_activity(old, new);
        let entries = merged.get("lead").unwrap();
        assert!(
            entries[0].completed_at.is_none(),
            "In-progress entries should have no completed_at"
        );
    }

    // --- visible_tool_entries tests ---

    #[test]
    fn test_visible_tool_entries_empty() {
        let app = test_app();
        assert!(app.visible_tool_entries("lead").is_empty());
    }

    #[test]
    fn test_visible_tool_entries_in_progress_always_visible() {
        let mut app = test_app();
        app.tool_activity = std::collections::HashMap::from([(
            "lead".to_string(),
            vec![ToolActivityEntry {
                header: "\u{203a} Read foo.rs".to_string(),
                completed_at: None,
            }],
        )]);
        let result = app.visible_tool_entries("lead");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].header, "\u{203a} Read foo.rs");
    }

    #[test]
    fn test_visible_tool_entries_completed_recent_visible() {
        let mut app = test_app();
        app.tool_activity = std::collections::HashMap::from([(
            "lead".to_string(),
            vec![ToolActivityEntry {
                header: "\u{2713} Read foo.rs".to_string(),
                completed_at: Some(std::time::Instant::now()),
            }],
        )]);
        let result = app.visible_tool_entries("lead");
        assert_eq!(
            result.len(),
            1,
            "Recently completed entry should be visible"
        );
    }

    #[test]
    fn test_visible_tool_entries_aged_out_hidden() {
        let mut app = test_app();
        app.tool_activity = std::collections::HashMap::from([(
            "lead".to_string(),
            vec![ToolActivityEntry {
                header: "\u{2713} Read foo.rs".to_string(),
                completed_at: Some(std::time::Instant::now() - std::time::Duration::from_secs(61)),
            }],
        )]);
        let result = app.visible_tool_entries("lead");
        assert!(
            result.is_empty(),
            "Entry completed >30s ago should be hidden"
        );
    }

    #[test]
    fn test_visible_tool_entries_max_3_newest_first() {
        let mut app = test_app();
        let now = std::time::Instant::now();
        app.tool_activity = std::collections::HashMap::from([(
            "lead".to_string(),
            vec![
                ToolActivityEntry {
                    header: "\u{2713} call1".to_string(),
                    completed_at: Some(now),
                },
                ToolActivityEntry {
                    header: "\u{2713} call2".to_string(),
                    completed_at: Some(now),
                },
                ToolActivityEntry {
                    header: "\u{2713} call3".to_string(),
                    completed_at: Some(now),
                },
                ToolActivityEntry {
                    header: "\u{2713} call4".to_string(),
                    completed_at: Some(now),
                },
                ToolActivityEntry {
                    header: "\u{203a} call5".to_string(),
                    completed_at: None,
                },
            ],
        )]);
        let result = app.visible_tool_entries("lead");
        assert_eq!(result.len(), 3, "Max 3 entries");
        assert_eq!(
            result[0].header, "\u{203a} call5",
            "Newest (call5) should be first"
        );
        assert_eq!(result[1].header, "\u{2713} call4");
        assert_eq!(result[2].header, "\u{2713} call3");
    }

    #[test]
    fn test_set_channel_lead_thinking_inserts_entry() {
        let mut app = test_app();
        assert!(app.channel_lead_thinking.is_empty());
        app.set_channel_lead_thinking("myproject");
        assert!(
            app.channel_lead_thinking.contains_key("myproject"),
            "set_channel_lead_thinking should insert an entry for the channel"
        );
        let elapsed = app.channel_lead_thinking["myproject"].elapsed();
        assert!(
            elapsed < CHANNEL_LEAD_THINKING_TIMEOUT,
            "The inserted instant should be recent (elapsed={elapsed:?})"
        );
    }

    #[test]
    fn test_message_cache_key_changes_when_channel_thinking_set() {
        // The render cache key must include channel_lead_thinking state because
        // it affects lead_indicator_height (0 -> 1), changing the message area layout.
        let mut app = test_app();
        app.selected_channel = "myproject".to_string();
        let key_before = app.message_cache_key(80, 24);
        app.set_channel_lead_thinking("myproject");
        let key_after = app.message_cache_key(80, 24);
        assert_ne!(
            key_before, key_after,
            "Cache key should change when channel_lead_thinking is set for the selected channel"
        );
    }

    #[test]
    fn test_clear_channel_lead_thinking_not_cleared_by_completed_entries() {
        // Optimistic thinking state must NOT be cleared when tool_activity only has
        // completed entries. The spinner should persist until an InProgress entry arrives.
        let mut app = test_app();
        app.set_channel_lead_thinking("myproject");

        // Add only completed (aged-out) entries for the channel
        app.tool_activity.insert(
            "myproject".to_string(),
            vec![ToolActivityEntry {
                header: "\u{2713} Read foo.rs".to_string(),
                completed_at: Some(std::time::Instant::now()),
            }],
        );

        app.clear_channel_lead_thinking_for_in_progress();

        assert!(
            app.channel_lead_thinking.contains_key("myproject"),
            "Thinking should NOT be cleared when tool_activity only has completed entries"
        );
    }

    #[test]
    fn test_clear_channel_lead_thinking_cleared_by_in_progress_entry() {
        // Optimistic thinking state MUST be cleared when an InProgress entry arrives,
        // since the channel lead has started responding.
        let mut app = test_app();
        app.set_channel_lead_thinking("myproject");

        app.tool_activity.insert(
            "myproject".to_string(),
            vec![ToolActivityEntry {
                header: "\u{203a} Write bar.rs".to_string(),
                completed_at: None, // InProgress
            }],
        );

        app.clear_channel_lead_thinking_for_in_progress();

        assert!(
            !app.channel_lead_thinking.contains_key("myproject"),
            "Thinking should be cleared when an InProgress tool entry exists"
        );
    }
}
