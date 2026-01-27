//! Application state and logic for the chat TUI

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use midtown::{Channel, Message};

/// A task item for the kanban board
#[derive(Debug, Clone)]
pub struct KanbanTask {
    pub id: String,
    pub subject: String,
    pub owner: Option<String>,
    pub status: TaskStatus,
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
}

/// A merged PR item for the Done column
#[derive(Debug, Clone)]
pub struct MergedPr {
    pub number: u64,
    pub title: String,
    pub merged_at: DateTime<Utc>,
}

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
    /// Last known message count (for detecting new messages)
    last_count: usize,
    /// Tasks for the kanban board
    pub tasks: Vec<KanbanTask>,
    /// Open PRs for the kanban board (Review column)
    pub prs: Vec<KanbanPr>,
    /// Merged PRs for the Done column
    pub merged_prs: Vec<MergedPr>,
    /// Repository name with owner (e.g., "btucker/midtown")
    pub repo_name: String,
    /// Last time kanban data was refreshed
    kanban_last_refresh: Instant,
}

/// Interval between kanban data refreshes (30 seconds)
const KANBAN_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

impl App {
    pub fn new() -> Self {
        // Determine the repo name from current directory (for channel)
        let dir_name = std::env::current_dir()
            .ok()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
            .unwrap_or_else(|| "default".to_string());

        let channel = Channel::for_repo(&dir_name).ok();

        // Get repo name with owner from gh CLI (e.g., "btucker/midtown")
        let repo_name = fetch_repo_name();

        let mut app = Self {
            messages: Vec::new(),
            scroll_offset: 0,
            visible_height: 20,
            channel,
            last_count: 0,
            tasks: Vec::new(),
            prs: Vec::new(),
            merged_prs: Vec::new(),
            repo_name,
            kanban_last_refresh: Instant::now() - KANBAN_REFRESH_INTERVAL, // Force initial refresh
        };

        // Initial load
        app.refresh();
        app
    }

    /// Refresh messages from the channel and kanban data
    pub fn refresh(&mut self) {
        // Read all messages from channel
        if let Some(ref channel) = self.channel
            && let Ok(messages) = channel.read_all()
        {
            let new_count = messages.len();

            // Update messages if count changed
            if new_count != self.last_count {
                let added = new_count.saturating_sub(self.last_count);
                let was_at_bottom = self.scroll_offset == 0;

                self.messages = messages;
                self.last_count = new_count;

                if was_at_bottom {
                    // User was at bottom - stay at bottom (auto-scroll)
                    self.scroll_offset = 0;
                } else {
                    // User had scrolled up - adjust offset to stay viewing same messages
                    self.scroll_offset += added;
                }
            }
        }

        // Refresh kanban data less frequently to avoid UI lag
        if self.kanban_last_refresh.elapsed() >= KANBAN_REFRESH_INTERVAL {
            self.refresh_kanban();
            self.kanban_last_refresh = Instant::now();
        }
    }

    /// Refresh kanban board data (tasks and PRs)
    fn refresh_kanban(&mut self) {
        self.tasks = fetch_tasks();
        self.prs = fetch_prs();
        self.merged_prs = fetch_merged_prs();
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
    }

    /// Page down
    pub fn page_down(&mut self) {
        let page_size = self.visible_height.saturating_sub(2);
        self.scroll_offset = self.scroll_offset.saturating_sub(page_size);
    }

    /// Scroll to top (oldest messages)
    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = self.max_scroll();
    }

    /// Scroll to bottom (newest messages)
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    /// Maximum scroll offset
    fn max_scroll(&self) -> usize {
        self.messages.len().saturating_sub(self.visible_height)
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

/// Fetch tasks from bd (beads) CLI
fn fetch_tasks() -> Vec<KanbanTask> {
    let mut tasks = Vec::new();

    // Get all tasks with bd list --json
    if let Ok(output) = std::process::Command::new("bd")
        .args(["list", "--json"])
        .output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Ok(beads) = serde_json::from_str::<Vec<serde_json::Value>>(&stdout) {
            for bead in beads {
                let id = bead
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let subject = bead
                    .get("subject")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let owner = bead.get("owner").and_then(|v| v.as_str()).map(String::from);
                let status_str = bead
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("open");

                let status = match status_str {
                    "in_progress" => TaskStatus::InProgress,
                    "completed" | "closed" => TaskStatus::Completed,
                    _ => TaskStatus::Pending,
                };

                if !id.is_empty() {
                    tasks.push(KanbanTask {
                        id,
                        subject,
                        owner,
                        status,
                    });
                }
            }
        }
    }

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

/// Fetch open PRs from GitHub using gh CLI
fn fetch_prs() -> Vec<KanbanPr> {
    let mut prs = Vec::new();

    if let Ok(output) = std::process::Command::new("gh")
        .args(["pr", "list", "--json", "number,title,author,createdAt"])
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
                let author = pr
                    .get("author")
                    .and_then(|v| v.get("login"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let created_at = pr
                    .get("createdAt")
                    .and_then(|v| v.as_str())
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(Utc::now);

                if number > 0 {
                    prs.push(KanbanPr {
                        number,
                        title,
                        author,
                        created_at,
                    });
                }
            }
        }
    }

    prs
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
