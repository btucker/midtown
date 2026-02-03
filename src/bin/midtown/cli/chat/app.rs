//! Application state and logic for the chat TUI

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use midtown::{Channel, Message};

use crate::client::DaemonClient;

/// Data fetched from background thread for kanban refresh
struct KanbanData {
    prs: Vec<KanbanPr>,
    merged_prs: Vec<MergedPr>,
    /// Repo metadata from daemon RPC (label, full_name)
    repos: Vec<(String, String)>,
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
    pub modified_at: Option<DateTime<Utc>>,
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
}

/// A merged PR item for the Done column
#[derive(Debug, Clone)]
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
    /// Daemon client for posting messages (enables nudge functionality)
    daemon_client: Option<DaemonClient>,
    /// Whether initial messages have been loaded
    initial_load_done: bool,
    /// Byte position where loaded history starts (0 means all history loaded)
    history_start_position: u64,
    /// Whether all history has been loaded
    history_fully_loaded: bool,
    /// Tasks for the kanban board
    pub tasks: Vec<KanbanTask>,
    /// Open PRs for the kanban board (Review column)
    pub prs: Vec<KanbanPr>,
    /// Merged PRs for the Done column
    pub merged_prs: Vec<MergedPr>,
    /// Repository name with owner (e.g., "btucker/midtown")
    /// Used for constructing GitHub PR URLs in kanban hyperlinks
    pub repo_name: String,
    /// Last time kanban data was refreshed
    kanban_last_refresh: Instant,
    /// Receiver for async kanban data from background thread
    kanban_receiver: Option<Receiver<KanbanData>>,
    /// Repository status (commit, CI, release info) - primary repo
    pub repo_status: RepoStatus,
    /// Multi-repo statuses (label, full_name, status) for all project repos
    pub repo_statuses: Vec<(RepoInfo, RepoStatus)>,
    /// Last time repo status was refreshed
    repo_status_last_refresh: Instant,
    /// Receiver for async repo status from background thread
    repo_status_receiver: Option<Receiver<Vec<(RepoInfo, RepoStatus)>>>,
    /// Selection mode - when true, mouse capture is disabled for text selection
    pub selection_mode: bool,
    /// Input mode - when true, keyboard input goes to the text input
    pub input_mode: bool,
    /// Current text in the input field
    pub input_text: String,
    /// Cursor position within input_text
    pub input_cursor: usize,
    /// User display name from config (None = "user")
    pub user_display_name: Option<String>,
    /// Cached mapping of coworker name -> current task subject.
    /// Rebuilt only when tasks change, not every frame.
    current_tasks_cache: HashMap<String, String>,
    /// Hash of task state used to detect when cache needs rebuilding
    tasks_cache_hash: u64,
}

/// Interval between kanban data refreshes (30 seconds)
const KANBAN_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Interval between repo status refreshes (60 seconds)
const REPO_STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

impl App {
    pub fn new() -> Self {
        // Use detect_repo_name() which correctly handles worktrees by using
        // git-common-dir, ensuring we read from the same channel as the daemon
        let channel_repo =
            midtown::paths::detect_repo_name().unwrap_or_else(|| "default".to_string());

        let channel = Channel::for_repo(&channel_repo).ok();

        // Connect to daemon for posting messages (optional - falls back to direct write)
        let daemon_client = DaemonClient::connect().ok();

        // Get repo name with owner from gh CLI (e.g., "btucker/midtown")
        let repo_name = fetch_repo_name();

        let mut app = Self {
            messages: VecDeque::new(),
            scroll_offset: 0,
            visible_height: 20,
            channel,
            daemon_client,
            initial_load_done: false,
            history_start_position: 0,
            history_fully_loaded: false,
            tasks: Vec::new(),
            prs: Vec::new(),
            merged_prs: Vec::new(),
            repo_name,
            kanban_last_refresh: Instant::now() - KANBAN_REFRESH_INTERVAL, // Force initial refresh
            kanban_receiver: None,
            repo_status: RepoStatus::default(),
            repo_statuses: Vec::new(),
            repo_status_last_refresh: Instant::now() - REPO_STATUS_REFRESH_INTERVAL, // Force initial refresh
            repo_status_receiver: None,
            selection_mode: false,
            input_mode: false,
            input_text: String::new(),
            input_cursor: 0,
            user_display_name: midtown::config::get_user_display_name(),
            current_tasks_cache: HashMap::new(),
            tasks_cache_hash: 0,
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
    }

    /// Refresh kanban board data (tasks and PRs)
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

    /// Scroll up one line
    pub fn scroll_up(&mut self) {
        let max_scroll = self.max_scroll();
        if self.scroll_offset < max_scroll {
            self.scroll_offset += 1;
        }
        self.maybe_load_more_history();
    }

    /// Scroll down one line
    pub fn scroll_down(&mut self) {
        if self.scroll_offset > 0 {
            self.scroll_offset -= 1;
        }
    }

    /// Page up
    pub fn page_up(&mut self) {
        let page_size = self.visible_height.saturating_sub(2);
        let max_scroll = self.max_scroll();
        self.scroll_offset = (self.scroll_offset + page_size).min(max_scroll);
        self.maybe_load_more_history();
    }

    /// Page down
    pub fn page_down(&mut self) {
        let page_size = self.visible_height.saturating_sub(2);
        self.scroll_offset = self.scroll_offset.saturating_sub(page_size);
    }

    /// Scroll to top (oldest messages)
    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = self.max_scroll();
        self.maybe_load_more_history();
    }

    /// Scroll to bottom (newest messages)
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    /// Toggle selection mode (disables mouse capture for text selection)
    pub fn toggle_selection_mode(&mut self) {
        self.selection_mode = !self.selection_mode;
    }

    /// Send the current input text as a message to the channel
    ///
    /// Prefers routing through the daemon RPC so the Lead gets nudged about
    /// new user messages. Falls back to direct channel write if daemon is unavailable.
    pub fn send_input(&mut self) {
        let text = self.input_text.trim().to_string();
        if text.is_empty() {
            return;
        }

        let sender = self.user_display_name.as_deref().unwrap_or("user");
        let sent = if let Some(ref client) = self.daemon_client {
            // Route through daemon so it can nudge the Lead
            client.channel_post_as(&text, sender).is_ok()
        } else if let Some(ref channel) = self.channel {
            // Fallback: direct write (won't trigger Lead nudge)
            let message = Message::text(sender, &text);
            channel.send(&message).is_ok()
        } else {
            false
        };

        if sent {
            self.input_text.clear();
            self.input_cursor = 0;
            // Refresh to pick up the new message
            self.refresh();
            self.scroll_to_bottom();
        }
    }

    /// Maximum scroll offset
    fn max_scroll(&self) -> usize {
        self.messages.len().saturating_sub(self.visible_height)
    }

    /// Check if we're at the maximum scroll position (viewing oldest messages).
    /// Used by the UI to determine line truncation strategy.
    pub fn is_at_max_scroll(&self) -> bool {
        let max = self.max_scroll();
        // At max scroll when scroll_offset >= max and we're actually scrolled
        max > 0 && self.scroll_offset >= max
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

            if !id.is_empty() {
                tasks.push(KanbanTask {
                    id,
                    subject,
                    owner,
                    status,
                    modified_at,
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

/// Fetch kanban PR data from the daemon via RPC.
///
/// Returns None if the daemon is not available, allowing fallback to direct gh CLI.
#[allow(clippy::type_complexity)]
fn fetch_kanban_data_via_rpc() -> Option<(Vec<KanbanPr>, Vec<MergedPr>, Vec<(String, String)>)> {
    use crate::client::DaemonClient;

    let client = DaemonClient::connect().ok()?;
    let data = client.kanban_data().ok()?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use midtown::Message;

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
            messages: VecDeque::new(),
            scroll_offset: 0,
            visible_height: 20,
            channel: None,
            daemon_client: None,
            initial_load_done: true,
            history_start_position: 0,
            history_fully_loaded: true,
            tasks: vec![
                KanbanTask {
                    id: "1".to_string(),
                    subject: "Pending task".to_string(),
                    owner: None,
                    status: TaskStatus::Pending,
                    modified_at: None,
                },
                KanbanTask {
                    id: "2".to_string(),
                    subject: "In progress task".to_string(),
                    owner: Some("park".to_string()),
                    status: TaskStatus::InProgress,
                    modified_at: None,
                },
                KanbanTask {
                    id: "3".to_string(),
                    subject: "Completed task".to_string(),
                    owner: Some("lexington".to_string()),
                    status: TaskStatus::Completed,
                    modified_at: None,
                },
            ],
            prs: Vec::new(),
            merged_prs: Vec::new(),
            repo_name: "test".to_string(),
            kanban_last_refresh: Instant::now(),
            kanban_receiver: None,
            repo_status: RepoStatus::default(),
            repo_statuses: Vec::new(),
            repo_status_last_refresh: Instant::now(),
            repo_status_receiver: None,
            selection_mode: false,
            input_mode: false,
            input_text: String::new(),
            input_cursor: 0,
            user_display_name: None,
            current_tasks_cache: HashMap::new(),
            tasks_cache_hash: 0,
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
            })
            .collect();

        let mut app = App {
            messages,
            scroll_offset: 0, // "at bottom" - should show most recent
            visible_height: 10,
            channel: None,
            daemon_client: None,
            initial_load_done: true,
            history_start_position: 0,
            history_fully_loaded: true,
            tasks: Vec::new(),
            prs: Vec::new(),
            merged_prs: Vec::new(),
            repo_name: "test".to_string(),
            kanban_last_refresh: Instant::now(),
            kanban_receiver: None,
            repo_status: RepoStatus::default(),
            repo_statuses: Vec::new(),
            repo_status_last_refresh: Instant::now(),
            repo_status_receiver: None,
            selection_mode: false,
            input_mode: false,
            input_text: String::new(),
            input_cursor: 0,
            user_display_name: None,
            current_tasks_cache: HashMap::new(),
            tasks_cache_hash: 0,
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
        use std::time::Instant;

        // Create an App with 100 messages and visible_height of 20
        let messages: VecDeque<Message> = (0..100)
            .map(|i| Message {
                id: i.to_string(),
                from: "test".to_string(),
                content: format!("message {}", i),
                timestamp: chrono::Utc::now(),
                message_type: midtown::MessageType::Text,
            })
            .collect();

        let mut app = App {
            messages,
            scroll_offset: 0,
            visible_height: 20,
            channel: None,
            daemon_client: None,
            initial_load_done: true,
            history_start_position: 0,
            history_fully_loaded: true,
            tasks: Vec::new(),
            prs: Vec::new(),
            merged_prs: Vec::new(),
            repo_name: "test".to_string(),
            kanban_last_refresh: Instant::now(),
            kanban_receiver: None,
            repo_status: RepoStatus::default(),
            repo_statuses: Vec::new(),
            repo_status_last_refresh: Instant::now(),
            repo_status_receiver: None,
            selection_mode: false,
            input_mode: false,
            input_text: String::new(),
            input_cursor: 0,
            user_display_name: None,
            current_tasks_cache: HashMap::new(),
            tasks_cache_hash: 0,
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

        // At max scroll (100 - 20 = 80): should be at max
        app.scroll_offset = 80;
        assert!(app.is_at_max_scroll(), "scroll_offset=80 should be at max");

        // Beyond max scroll: should still be considered at max
        app.scroll_offset = 85;
        assert!(
            app.is_at_max_scroll(),
            "scroll_offset=85 (beyond max) should be at max"
        );
    }
}
