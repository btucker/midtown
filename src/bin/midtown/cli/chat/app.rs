//! Application state and logic for the chat TUI

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
}

/// A merged PR item for the Done column
#[derive(Debug, Clone)]
pub struct MergedPr {
    pub number: u64,
    pub title: String,
    pub merged_at: DateTime<Utc>,
}

/// Number of messages to load initially and per history load
const INITIAL_MESSAGE_COUNT: usize = 100;

/// Application state
pub struct App {
    /// All messages from the channel
    pub messages: Vec<Message>,
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
    /// Repository status (commit, CI, release info)
    pub repo_status: RepoStatus,
    /// Last time repo status was refreshed
    repo_status_last_refresh: Instant,
    /// Receiver for async repo status from background thread
    repo_status_receiver: Option<Receiver<RepoStatus>>,
    /// Selection mode - when true, mouse capture is disabled for text selection
    pub selection_mode: bool,
    /// Input mode - when true, keyboard input goes to the text input
    pub input_mode: bool,
    /// Current text in the input field
    pub input_text: String,
    /// Cursor position within input_text
    pub input_cursor: usize,
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
            messages: Vec::new(),
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
            repo_status_last_refresh: Instant::now() - REPO_STATUS_REFRESH_INTERVAL, // Force initial refresh
            repo_status_receiver: None,
            selection_mode: false,
            input_mode: false,
            input_text: String::new(),
            input_cursor: 0,
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
                    self.messages = messages;
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
                Ok(status) => {
                    self.repo_status = status;
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

        // PRs require gh CLI calls - run in background thread to avoid blocking UI
        let (tx, rx) = mpsc::channel();
        self.kanban_receiver = Some(rx);

        thread::spawn(move || {
            let prs = fetch_prs();
            let merged_prs = fetch_merged_prs();
            // Ignore send error if receiver dropped (app closed)
            let _ = tx.send(KanbanData { prs, merged_prs });
        });
    }

    /// Refresh repository status (commit, CI, release)
    fn refresh_repo_status(&mut self) {
        let (tx, rx) = mpsc::channel();
        self.repo_status_receiver = Some(rx);

        thread::spawn(move || {
            let status = fetch_repo_status();
            let _ = tx.send(status);
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

        let sent = if let Some(ref client) = self.daemon_client {
            // Route through daemon so it can nudge the Lead
            client.channel_post_as(&text, "user").is_ok()
        } else if let Some(ref channel) = self.channel {
            // Fallback: direct write (won't trigger Lead nudge)
            let message = Message::text("user", &text);
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

    /// Check if user is near the top of loaded messages
    fn is_near_top(&self) -> bool {
        let max = self.max_scroll();
        // Consider "near top" if within 10 messages of the oldest loaded
        self.scroll_offset >= max.saturating_sub(10)
    }

    /// Load more history if user scrolls near the top
    fn maybe_load_more_history(&mut self) {
        if self.history_fully_loaded || !self.is_near_top() {
            return;
        }

        if let Some(ref channel) = self.channel
            && let Ok((older_messages, new_start)) = channel
                .read_messages_before_position(self.history_start_position, INITIAL_MESSAGE_COUNT)
        {
            if !older_messages.is_empty() {
                let added = older_messages.len();
                // Prepend older messages to the beginning
                let mut combined = older_messages;
                combined.extend(std::mem::take(&mut self.messages));
                self.messages = combined;

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
    pub fn visible_messages(&self) -> &[Message] {
        let total = self.messages.len();
        if total == 0 {
            return &[];
        }

        // scroll_offset=0 means we show the most recent messages (end of list)
        // Higher scroll_offset means we show older messages
        let end = total.saturating_sub(self.scroll_offset);
        let start = end.saturating_sub(self.visible_height);

        &self.messages[start..end]
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
            "number,title,author,createdAt,body,statusCheckRollup",
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

                if number > 0 {
                    prs.push(KanbanPr {
                        number,
                        title,
                        author,
                        created_at,
                        ci_status,
                    });
                }
            }
        }
    }

    prs
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
                    });
                }
            }
        }
    }

    // Sort by merged_at descending (most recent first)
    prs.sort_by(|a, b| b.merged_at.cmp(&a.merged_at));
    prs
}

/// Fetch repository status (commit, CI status, release) from GitHub using gh CLI
fn fetch_repo_status() -> RepoStatus {
    let mut status = RepoStatus::default();

    // Fetch latest commit on default branch
    if let Ok(output) = std::process::Command::new("gh")
        .args([
            "api",
            "repos/{owner}/{repo}/commits/{branch}",
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
            "repos/{owner}/{repo}/actions/runs?branch=main&per_page=1",
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
            "repos/{owner}/{repo}/releases/latest",
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
            messages: Vec::new(),
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
            repo_status_last_refresh: Instant::now(),
            repo_status_receiver: None,
            selection_mode: false,
            input_mode: false,
            input_text: String::new(),
            input_cursor: 0,
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
        let messages: Vec<Message> = (0..50)
            .map(|i| Message {
                id: i.to_string(),
                from: "test".to_string(),
                content: format!("message {}", i),
                timestamp: chrono::Utc::now(),
                message_type: midtown::MessageType::Text,
            })
            .collect();

        let app = App {
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
            repo_status_last_refresh: Instant::now(),
            repo_status_receiver: None,
            selection_mode: false,
            input_mode: false,
            input_text: String::new(),
            input_cursor: 0,
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
}
