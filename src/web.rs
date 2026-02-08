//! Web API server for Svelte mobile app
//!
//! Provides API endpoints and WebSocket connections for live updates
//! (channel messages, coworker status, etc.). Static files are served
//! only by the shared gateway on port 47022.

use axum::{
    Json, Router,
    extract::{
        Query, State,
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info, warn};

use crate::channel::Channel;
use crate::coworker::CoworkerManager;
use crate::message::Message;
use crate::push::PushManager;
use crate::tasks::extract_task_id_from_pr_title;
use crate::tmux;

/// Tracks which WebSocket connections are viewing which tmux windows,
/// along with each viewer's viewport width in columns.
///
/// NOTE: This tracker no longer performs any tmux resizing. The resize
/// functionality was removed because it blocked TUI users from controlling
/// their terminal size. The tracker is retained for potential future
/// analytics, debugging, or optional resize features.
#[derive(Debug)]
pub struct ViewerTracker {
    /// Map of conn_id → (window_name, cols)
    viewers: std::collections::HashMap<u64, (String, u16)>,
    /// Next connection ID to assign
    next_conn_id: u64,
    /// Tmux session name for resize commands
    session: String,
}

impl ViewerTracker {
    pub fn new(session: String) -> Self {
        Self {
            viewers: std::collections::HashMap::new(),
            next_conn_id: 1,
            session,
        }
    }

    /// Allocate a new connection ID.
    pub fn new_conn_id(&mut self) -> u64 {
        let id = self.next_conn_id;
        self.next_conn_id += 1;
        id
    }

    /// Register or update a viewer's window and viewport width.
    ///
    /// Returns ResizeActions (currently unused - resize execution is disabled).
    pub fn set_viewing(&mut self, conn_id: u64, window: String, cols: u16) -> ResizeActions {
        let old_window = self.viewers.get(&conn_id).map(|(w, _)| w.clone());
        self.viewers.insert(conn_id, (window.clone(), cols));

        let mut actions = ResizeActions::default();

        // If viewer switched windows, check if old window needs reset
        if let Some(ref old) = old_window
            && *old != window
        {
            let max_cols = self.max_cols_for_window(old);
            if max_cols == 0 {
                actions.reset_windows.push(old.clone());
            } else {
                actions.resize_windows.push((old.clone(), max_cols));
            }
        }

        // Resize the new window to the max viewer width
        let max_cols = self.max_cols_for_window(&window);
        actions.resize_windows.push((window, max_cols));

        actions
    }

    /// Remove a viewer (on disconnect or leave).
    ///
    /// Returns ResizeActions (currently unused - resize execution is disabled).
    pub fn remove_viewer(&mut self, conn_id: u64) -> ResizeActions {
        let mut actions = ResizeActions::default();

        if let Some((window, _)) = self.viewers.remove(&conn_id) {
            let max_cols = self.max_cols_for_window(&window);
            if max_cols == 0 {
                actions.reset_windows.push(window);
            } else {
                actions.resize_windows.push((window, max_cols));
            }
        }

        actions
    }

    /// Stop viewing without removing the connection (viewer navigated away).
    pub fn stop_viewing(&mut self, conn_id: u64) -> ResizeActions {
        self.remove_viewer(conn_id)
    }

    /// Compute max(cols) across all viewers of a given window.
    fn max_cols_for_window(&self, window: &str) -> u16 {
        self.viewers
            .values()
            .filter(|(w, _)| w == window)
            .map(|(_, cols)| *cols)
            .max()
            .unwrap_or(0)
    }
}

/// Actions to perform after a viewer change.
#[derive(Debug, Default)]
pub struct ResizeActions {
    /// Windows to resize to a specific column width.
    pub resize_windows: Vec<(String, u16)>,
    /// Windows to reset to automatic sizing.
    pub reset_windows: Vec<String>,
}

impl ResizeActions {
    /// Execute all resize/reset actions against tmux.
    ///
    /// NOTE: This is intentionally a no-op. The TUI user controls tmux sizing,
    /// and the web UI adapts to whatever size the terminal is. Web-driven
    /// resizing was removed because it blocked TUI users from resizing their
    /// terminal (tmux resize-window commands would override their changes).
    ///
    /// The ViewerTracker is retained for potential future analytics/debugging,
    /// but no actual resize commands are executed.
    #[allow(unused_variables)]
    pub fn execute(&self, session: &str) {
        // Intentionally empty - see doc comment above.
        // Previously this resized tmux windows to match web viewer viewport,
        // but that prevented TUI users from controlling their terminal size.
    }
}

/// TTL for cached API responses (30 seconds).
const CACHE_TTL: Duration = Duration::from_secs(30);

/// Thread-safe TTL cache for expensive API responses.
///
/// Stores a timestamped value behind a mutex. Callers check staleness
/// and refresh only when the cached entry has expired.
struct TtlCache<T> {
    inner: Mutex<Option<(Instant, T)>>,
}

impl<T: Clone> TtlCache<T> {
    const fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    /// Return cached value if it exists and is younger than `ttl`.
    fn get(&self, ttl: Duration) -> Option<T> {
        let guard = self.inner.lock().ok()?;
        guard
            .as_ref()
            .filter(|(ts, _)| ts.elapsed() < ttl)
            .map(|(_, v)| v.clone())
    }

    /// Store a new value with the current timestamp.
    fn set(&self, value: T) {
        if let Ok(mut guard) = self.inner.lock() {
            *guard = Some((Instant::now(), value));
        }
    }
}

/// Cached repo status (commit, CI, release).
static REPO_STATUS_CACHE: TtlCache<RepoStatus> = TtlCache::new();

/// Cached open PR list.
static OPEN_PRS_CACHE: TtlCache<Vec<serde_json::Value>> = TtlCache::new();

/// Cached merged PR list.
static MERGED_PRS_CACHE: TtlCache<Vec<serde_json::Value>> = TtlCache::new();

/// Cached usage data (session + weekly utilization).
static USAGE_CACHE: TtlCache<crate::usage::UsageData> = TtlCache::new();

/// TTL for usage data cache (2 minutes, matching TUI refresh interval).
const USAGE_CACHE_TTL: Duration = Duration::from_secs(120);

/// Configuration for the web server
#[derive(Debug, Clone)]
pub struct WebConfig {
    /// Repository name for channel access
    pub repo: String,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            repo: "default".to_string(),
        }
    }
}

/// A mobile channel post to be forwarded to the daemon for processing.
pub struct MobileChannelPost {
    pub content: String,
}

/// Shared state for WebSocket connections
pub struct WebState {
    pub config: WebConfig,
    /// Broadcast channel for real-time updates
    pub updates_tx: broadcast::Sender<WebUpdate>,
    /// Coworker manager for querying live coworker state
    pub coworkers: Option<CoworkerManager>,
    /// Sender for channel posts to be processed by the daemon
    pub channel_post_tx: mpsc::Sender<MobileChannelPost>,
    /// Web Push notification manager (shared with daemon)
    pub push_manager: Option<Arc<PushManager>>,
    /// Paths to all repos in the project (for multi-repo PR URL resolution)
    pub all_repo_paths: Vec<std::path::PathBuf>,
    /// Default branch name (e.g. "main" or "master")
    pub default_branch: String,
    /// Cached GitHub repo full names (owner/repo) by repo path.
    /// Repo names never change during a session, so we cache indefinitely.
    pub repo_name_cache: std::sync::RwLock<std::collections::HashMap<std::path::PathBuf, String>>,
    /// Tracks which WebSocket connections are viewing which tmux windows.
    /// Note: Resize functionality is disabled; TUI users control terminal size.
    pub viewer_tracker: Mutex<ViewerTracker>,
}

impl WebState {
    /// Get the GitHub full name (owner/repo) for a repo path, using cache.
    ///
    /// On first call for a given path, runs `gh repo view --json nameWithOwner`.
    /// Subsequent calls return the cached value without any API call.
    fn get_repo_full_name(&self, repo_path: &std::path::Path) -> String {
        // Fast path: check cache
        {
            let cache = self.repo_name_cache.read().unwrap();
            if let Some(name) = cache.get(repo_path) {
                return name.clone();
            }
        }
        // Slow path: fetch from GitHub CLI and cache
        let name = std::process::Command::new("gh")
            .current_dir(repo_path)
            .args([
                "repo",
                "view",
                "--json",
                "nameWithOwner",
                "--jq",
                ".nameWithOwner",
            ])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        let mut cache = self.repo_name_cache.write().unwrap();
        cache.insert(repo_path.to_path_buf(), name.clone());
        name
    }
}

/// Types of real-time updates sent to clients
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "data")]
pub enum WebUpdate {
    /// New channel message
    #[serde(rename = "channel_message")]
    ChannelMessage(ChannelMessageData),
    /// Coworker status changed
    #[serde(rename = "coworker_status")]
    CoworkerStatus(CoworkerStatusData),
    /// Lead is actively working (typing indicator)
    #[serde(rename = "lead_typing")]
    LeadTyping(LeadTypingData),
    /// Error response for a client action
    #[serde(rename = "error")]
    Error(ErrorData),
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelMessageData {
    pub from: String,
    pub content: String,
    pub timestamp: String,
    #[serde(rename = "type")]
    pub msg_type: String,
    /// Channel name (defaults to "midtown" for backward compat)
    #[serde(default = "default_channel")]
    pub channel: String,
}

#[allow(dead_code)] // Used by serde default attribute
fn default_channel() -> String {
    "midtown".to_string()
}

#[derive(Debug, Clone, Serialize)]
pub struct CoworkerStatusData {
    pub name: String,
    pub status: String,
    pub current_task: Option<String>,
    pub model: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LeadTypingData {
    pub working: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorData {
    pub message: String,
}

/// WebSocket message from client
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    /// Send a message to the channel (to lead)
    #[serde(rename = "send_message")]
    SendMessage { content: String },
    /// Request full channel history
    #[serde(rename = "get_history")]
    GetHistory,
    /// Request coworker status
    #[serde(rename = "get_status")]
    GetStatus,
    /// Client is viewing a tmux window (resize disabled; for tracking only)
    #[serde(rename = "view_window")]
    ViewWindow { window: String, cols: u16 },
    /// Client stopped viewing a tmux window (resize disabled; for tracking only)
    #[serde(rename = "leave_window")]
    LeaveWindow,
    /// Send a nudge (text input) to a coworker or the lead
    #[serde(rename = "nudge")]
    Nudge { target: String, message: String },
    /// Send a special key (like Escape) to a coworker or the lead
    #[serde(rename = "send_key")]
    SendKey { target: String, key: String },
}

/// Create the web server router
///
/// This can be nested into the main webhook server.
/// Only serves API endpoints — static files are handled by the shared gateway.
pub fn create_web_router(state: Arc<WebState>) -> Router {
    Router::new()
        .route("/api/ws", get(ws_handler))
        .route("/api/health", get(api_health))
        .route("/api/channel", get(api_channel_history))
        .route("/api/status", get(api_status))
        .route("/api/lead-pane", get(api_lead_pane))
        .route("/api/tmux-pane", get(api_tmux_pane))
        .route("/api/tmux-windows", get(api_tmux_windows))
        .route("/api/push/vapid-key", get(api_push_vapid_key))
        .route("/api/push/subscribe", post(api_push_subscribe))
        .route("/api/push/unsubscribe", post(api_push_unsubscribe))
        .route("/api/auth/profiles", get(api_auth_profiles))
        .route("/api/auth/switch", post(api_auth_switch))
        .route("/api/usage", get(api_usage))
        .with_state(state)
}

/// Health check endpoint
async fn api_health() -> &'static str {
    "ok"
}

/// Get channel message history
async fn api_channel_history(
    State(state): State<Arc<WebState>>,
) -> Result<impl IntoResponse, StatusCode> {
    let channel = Channel::for_repo(&state.config.repo).map_err(|e| {
        error!("Failed to open channel: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let messages = channel.read_all().map_err(|e| {
        error!("Failed to read channel: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let response: Vec<ChannelMessageData> = messages
        .into_iter()
        .map(|m| {
            let channel = m.channel_name().to_string();
            ChannelMessageData {
                from: m.from,
                content: m.content,
                timestamp: m.timestamp.to_rfc3339(),
                msg_type: format!("{:?}", m.message_type).to_lowercase(),
                channel,
            }
        })
        .collect();

    Ok(axum::Json(response))
}

/// CI status for the repository
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CiStatus {
    Passed,
    Failed,
    Running,
    Unknown,
}

/// Repository status information (commit, CI, release)
#[derive(Debug, Clone, Serialize, Default)]
pub struct RepoStatus {
    pub commit_hash: String,
    pub commit_time: Option<String>,
    pub ci_status: Option<CiStatus>,
    pub release_tag: Option<String>,
    pub release_time: Option<String>,
}

/// Fetch kanban data (PRs + merged PRs) from the daemon via RPC.
///
/// Connects to the daemon's Unix socket and calls `kanban.data`, which uses
/// a single batched GraphQL query internally. Returns `None` if the daemon
/// is unreachable or the response is unexpected.
fn fetch_kanban_via_rpc(repo: &str) -> Option<(Vec<serde_json::Value>, Vec<serde_json::Value>)> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let socket = crate::paths::daemon_socket_for_repo(repo);
    let mut stream = UnixStream::connect(&socket).ok()?;
    // Set a timeout so we don't block forever if daemon is busy
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "kanban.data",
        "id": 1
    });
    writeln!(stream, "{}", request).ok()?;
    stream.flush().ok()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;

    let resp: serde_json::Value = serde_json::from_str(&line).ok()?;
    let result = resp.get("result")?;

    let prs = result.get("prs")?.as_array()?.clone();
    let merged = result.get("merged_prs")?.as_array()?.clone();
    Some((prs, merged))
}

/// Fetch repository status via gh CLI
fn fetch_repo_status(default_branch: &str) -> RepoStatus {
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
                status.commit_time = Some(date_str.to_string());
            }
        }
    }

    // Fetch CI status from latest workflow run on default branch
    let actions_url = format!(
        "repos/{{owner}}/{{repo}}/actions/runs?branch={}&per_page=1",
        default_branch
    );
    if let Ok(output) = std::process::Command::new("gh")
        .args([
            "api",
            &actions_url,
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

            status.ci_status = Some(match (run_status, conclusion) {
                ("completed", Some("success")) => CiStatus::Passed,
                ("completed", Some("failure")) => CiStatus::Failed,
                ("completed", Some("cancelled")) => CiStatus::Failed,
                ("in_progress", _) | ("queued", _) | ("waiting", _) => CiStatus::Running,
                _ => CiStatus::Unknown,
            });
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
                status.release_time = Some(date_str.to_string());
            }
        }
    }

    status
}

/// Get daemon/coworker status including tasks and PRs for kanban board.
///
/// Uses TTL caches (30 s) and the daemon's `kanban.data` RPC to avoid
/// redundant GitHub API calls on every poll.
async fn api_status(State(state): State<Arc<WebState>>) -> Result<impl IntoResponse, StatusCode> {
    // Read tasks directly from Claude Code task storage (local file, cheap)
    let tasks: Vec<serde_json::Value> = crate::tasks::read_tasks()
        .into_iter()
        .map(|task| {
            let status = match task.status {
                crate::tasks::TaskStatus::Pending => "pending",
                crate::tasks::TaskStatus::InProgress => "in_progress",
                crate::tasks::TaskStatus::Completed => "completed",
            };
            serde_json::json!({
                "id": task.id,
                "subject": task.subject,
                "description": task.description,
                "status": status,
                "owner": task.owner,
            })
        })
        .collect();

    // Build a map of task_id -> task_subject for PR enrichment
    // Task.id is a String, so parse the JSON string value to u64
    let task_map: std::collections::HashMap<u64, String> = tasks
        .iter()
        .filter_map(|t| {
            let id_str = t.get("id").and_then(|i| i.as_str())?;
            let id = id_str.parse::<u64>().ok()?;
            let subject = t.get("subject").and_then(|s| s.as_str())?;
            Some((id, subject.to_string()))
        })
        .collect();

    // Load persistent state to get reviewer assignments (local file, cheap)
    let persistent_state =
        crate::daemon::state::DaemonPersistentState::load_for_repo(&state.config.repo)
            .unwrap_or_default();

    // --- PR data: prefer daemon RPC, fall back to cached gh CLI calls ---
    let repo_name = state.config.repo.clone();
    let (pull_requests, merged_prs) = tokio::task::spawn_blocking(move || {
        // Try daemon RPC first (single GraphQL call inside the daemon)
        if let Some((rpc_prs, rpc_merged)) = fetch_kanban_via_rpc(&repo_name) {
            return (rpc_prs, rpc_merged);
        }
        // Fall back to cached gh CLI calls
        let open = OPEN_PRS_CACHE.get(CACHE_TTL).unwrap_or_else(|| {
            let prs = fetch_open_prs_via_cli();
            OPEN_PRS_CACHE.set(prs.clone());
            prs
        });
        let merged = MERGED_PRS_CACHE.get(CACHE_TTL).unwrap_or_else(|| {
            let prs = fetch_merged_prs_via_cli();
            MERGED_PRS_CACHE.set(prs.clone());
            prs
        });
        (open, merged)
    })
    .await
    .unwrap_or_default();

    // Transform open PRs: enrich with reviewer info from persistent state
    let pull_requests: Vec<serde_json::Value> = pull_requests
        .into_iter()
        .map(|pr| {
            let pr_number = pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
            // RPC returns "ci_status" / "reviewer" / "reviewed_at"; gh CLI returns
            // "isDraft" / "reviewDecision". Handle both shapes.
            // Look up reviewer from persistent state (covers both RPC and CLI shapes)
            let assignment = persistent_state.github.pr_reviewers.get(&pr_number);
            let reviewer = pr
                .get("reviewer")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| assignment.map(|a| a.reviewer.clone()));
            let reviewer_assigned_at = assignment.map(|a| a.assigned_at.to_rfc3339());
            // Prefer review_posted from RPC response (computed from actual PR comments),
            // fall back to persistent local state for the CLI path
            let review_posted = pr
                .get("review_posted")
                .and_then(|v| v.as_bool())
                .unwrap_or_else(|| persistent_state.github.reviewed_prs.contains(&pr_number));
            let status = if pr.get("ci_status").is_some() {
                // RPC shape: derive review status from review_posted and reviewer
                if review_posted {
                    "reviewed"
                } else if reviewer.is_some() {
                    "awaiting review"
                } else {
                    "open"
                }
            } else {
                // gh CLI shape: use isDraft and reviewDecision fields
                let is_draft = pr.get("isDraft").and_then(|d| d.as_bool()).unwrap_or(false);
                if is_draft {
                    "draft"
                } else {
                    match pr
                        .get("reviewDecision")
                        .and_then(|r| r.as_str())
                        .unwrap_or("")
                    {
                        "APPROVED" => "approved",
                        "CHANGES_REQUESTED" => "changes requested",
                        "REVIEW_REQUIRED" => "awaiting review",
                        _ => "open",
                    }
                }
            };
            // Extract author: RPC uses flat "author" string, CLI uses {"login": ...}
            let author = pr
                .get("author")
                .and_then(|a| {
                    a.as_str().map(|s| s.to_string()).or_else(|| {
                        a.get("login")
                            .and_then(|l| l.as_str())
                            .map(|s| s.to_string())
                    })
                })
                .unwrap_or_else(|| "unknown".to_string());
            let created_at = pr
                .get("createdAt")
                .or_else(|| pr.get("created_at"))
                .and_then(|c| c.as_str());
            // Extract task ID from PR title and look up task name
            let title = pr.get("title").and_then(|t| t.as_str()).unwrap_or("");
            let task_id = extract_task_id_from_pr_title(title);
            let task_name = task_id.and_then(|id| task_map.get(&id).cloned());
            serde_json::json!({
                "number": pr_number,
                "title": title,
                "author": author,
                "status": status,
                "reviewer": reviewer,
                "reviewer_assigned_at": reviewer_assigned_at,
                "review_posted": review_posted,
                "created_at": created_at,
                "task_id": task_id,
                "task_name": task_name,
            })
        })
        .collect();

    // Build a map of coworker name -> current task subject from in_progress tasks
    let coworker_tasks: std::collections::HashMap<String, String> = tasks
        .iter()
        .filter_map(|t| {
            let status = t.get("status").and_then(|s| s.as_str()).unwrap_or("");
            let owner = t.get("owner").and_then(|o| o.as_str()).unwrap_or("");
            let subject = t.get("subject").and_then(|s| s.as_str()).unwrap_or("");
            if status == "in_progress" && !owner.is_empty() {
                Some((owner.to_lowercase(), subject.to_string()))
            } else {
                None
            }
        })
        .collect();

    let coworkers_data: Vec<serde_json::Value> = state
        .coworkers
        .as_ref()
        .map(|mgr| {
            mgr.list()
                .into_iter()
                .map(|cw| {
                    // Look up current task from task storage (case-insensitive)
                    let current_task = coworker_tasks.get(&cw.name.to_lowercase()).cloned();
                    serde_json::json!({
                        "name": cw.name,
                        "status": cw.status.to_string(),
                        "current_task": current_task,
                        "started_at": cw.started_at.to_rfc3339(),
                        "model": cw.model,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    // Fetch repo status with TTL cache (blocking I/O)
    let default_branch = state.default_branch.clone();
    let repo_status = tokio::task::spawn_blocking(move || {
        REPO_STATUS_CACHE.get(CACHE_TTL).unwrap_or_else(|| {
            let status = fetch_repo_status(&default_branch);
            REPO_STATUS_CACHE.set(status.clone());
            status
        })
    })
    .await
    .unwrap_or_default();

    // Resolve the primary repo's full name (owner/repo) for PR link generation.
    let repo_full_name: String = state
        .all_repo_paths
        .first()
        .map(|repo_path| state.get_repo_full_name(repo_path))
        .unwrap_or_default();

    // Build repo metadata for multi-repo PR URL resolution
    let repo_statuses: Vec<serde_json::Value> = if state.all_repo_paths.len() > 1 {
        state
            .all_repo_paths
            .iter()
            .map(|repo_path| {
                let label = repo_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                let full_name = state.get_repo_full_name(repo_path);
                serde_json::json!({
                    "label": label,
                    "fullName": full_name,
                })
            })
            .collect()
    } else {
        Vec::new()
    };

    let status = serde_json::json!({
        "daemon": "running",
        "coworkers": coworkers_data,
        "tasks": tasks,
        "pull_requests": pull_requests,
        "merged_prs": merged_prs,
        "repo_name": state.config.repo,
        "repo_full_name": repo_full_name,
        "repo_status": repo_status,
        "repo_statuses": repo_statuses,
    });

    Ok(axum::Json(status))
}

/// Fetch open PRs via gh CLI (used as fallback when daemon RPC is unavailable).
fn fetch_open_prs_via_cli() -> Vec<serde_json::Value> {
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--json",
            "number,title,author,state,isDraft,reviewDecision,createdAt",
        ])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            serde_json::from_str(&stdout).unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

/// Fetch recently merged PRs via gh CLI (used as fallback when daemon RPC is unavailable).
fn fetch_merged_prs_via_cli() -> Vec<serde_json::Value> {
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--state",
            "merged",
            "--limit",
            "10",
            "--json",
            "number,title,mergedAt",
        ])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            serde_json::from_str(&stdout).unwrap_or_default()
        }
        _ => Vec::new(),
    }
}
/// Get the lead's tmux pane content
///
/// Captures the current content of the lead window's pane via tmux.
/// The frontend polls this endpoint to stream the lead's terminal output.
async fn api_lead_pane(
    State(state): State<Arc<WebState>>,
) -> Result<impl IntoResponse, StatusCode> {
    let session = format!("{}{}", tmux::SESSION_PREFIX, state.config.repo);
    let target = format!("{}:lead", session);

    // capture_pane is blocking (spawns a process), so run on blocking thread pool
    let content = tokio::task::spawn_blocking(move || tmux::capture_pane(&target))
        .await
        .map_err(|e| {
            error!("Failed to spawn blocking task: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    match content {
        Some(text) => Ok(axum::Json(serde_json::json!({ "content": text }))),
        None => Err(StatusCode::NOT_FOUND),
    }
}

/// Query parameters for tmux pane capture
#[derive(Debug, Deserialize)]
struct TmuxPaneQuery {
    /// Which tmux window to capture (e.g., "lead", "riverside")
    window: String,
}

/// Get the content of any tmux window's pane
///
/// Accepts a `?window=name` query parameter to select which window to capture.
/// Used by the "Tmux" tab in the web UI.
async fn api_tmux_pane(
    State(state): State<Arc<WebState>>,
    Query(params): Query<TmuxPaneQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let session = format!("{}{}", tmux::SESSION_PREFIX, state.config.repo);
    let window = params.window;

    // Validate window name: only allow non-empty alphanumeric, hyphens, and underscores
    if window.is_empty()
        || !window
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    let target = format!("{}:{}", session, window);

    let content = tokio::task::spawn_blocking(move || tmux::capture_pane(&target))
        .await
        .map_err(|e| {
            error!("Failed to spawn blocking task: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    match content {
        Some(text) => Ok(axum::Json(serde_json::json!({ "content": text }))),
        None => Err(StatusCode::NOT_FOUND),
    }
}

/// List all tmux windows in the session
///
/// Returns a JSON array of window names that can be passed to `/api/tmux-pane?window=`.
async fn api_tmux_windows(
    State(state): State<Arc<WebState>>,
) -> Result<impl IntoResponse, StatusCode> {
    let session = format!("{}{}", tmux::SESSION_PREFIX, state.config.repo);

    let windows = tokio::task::spawn_blocking(move || tmux::list_all_windows(&session))
        .await
        .map_err(|e| {
            error!("Failed to spawn blocking task: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .map_err(|e| {
            error!("Failed to list tmux windows: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(axum::Json(serde_json::json!({ "windows": windows })))
}

/// Get the VAPID public key for push subscription.
async fn api_push_vapid_key(
    State(state): State<Arc<WebState>>,
) -> Result<impl IntoResponse, StatusCode> {
    let push = state
        .push_manager
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    let key = push.vapid_public_key_base64().map_err(|e| {
        error!("Failed to get VAPID public key: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(axum::Json(serde_json::json!({ "publicKey": key })))
}

/// Subscribe request body from the browser.
#[derive(Debug, Deserialize)]
struct PushSubscribeRequest {
    endpoint: String,
    p256dh: String,
    auth: String,
}

/// Subscribe a client for push notifications.
async fn api_push_subscribe(
    State(state): State<Arc<WebState>>,
    Json(body): Json<PushSubscribeRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let push = state
        .push_manager
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    let sub = crate::push::PushSubscription {
        endpoint: body.endpoint,
        p256dh: body.p256dh,
        auth: body.auth,
    };

    push.add_subscription(sub).map_err(|e| {
        error!("Failed to store push subscription: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    info!("New push subscription registered");
    Ok(StatusCode::CREATED)
}

/// Unsubscribe request body.
#[derive(Debug, Deserialize)]
struct PushUnsubscribeRequest {
    endpoint: String,
}

/// Unsubscribe a client from push notifications.
async fn api_push_unsubscribe(
    State(state): State<Arc<WebState>>,
    Json(body): Json<PushUnsubscribeRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let push = state
        .push_manager
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    push.remove_subscription(&body.endpoint).map_err(|e| {
        error!("Failed to remove push subscription: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    info!("Push subscription removed");
    Ok(StatusCode::OK)
}

/// List all auth profiles with their status.
///
/// Returns a JSON array of `{name, is_current, has_credentials}`.
async fn api_auth_profiles() -> Result<impl IntoResponse, StatusCode> {
    let profiles = tokio::task::spawn_blocking(|| {
        let current = crate::auth::current_profile();
        let names = crate::auth::list_profiles().unwrap_or_default();
        names
            .into_iter()
            .map(|name| {
                let status = crate::auth::profile_status(&name);
                let has_credentials = status.as_ref().map(|s| s.has_credentials).unwrap_or(false);
                serde_json::json!({
                    "name": name,
                    "is_current": name == current,
                    "has_credentials": has_credentials,
                })
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| {
        error!("Failed to list auth profiles: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(axum::Json(profiles))
}

/// Request body for switching auth profiles.
#[derive(Debug, Deserialize)]
struct AuthSwitchRequest {
    profile: String,
}

/// Switch the active auth profile via the daemon's RPC.
///
/// Proxies to the daemon's `auth.switch` RPC, which shuts down all coworkers,
/// switches the profile on disk, and re-launches the lead.
async fn api_auth_switch(
    State(state): State<Arc<WebState>>,
    Json(body): Json<AuthSwitchRequest>,
) -> Result<impl IntoResponse, (StatusCode, axum::Json<serde_json::Value>)> {
    // Validate profile name at the API boundary before touching the RPC
    if let Err(e) = crate::auth::validate_profile_name(&body.profile) {
        return Err((
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({ "error": e.to_string() })),
        ));
    }

    let repo = state.config.repo.clone();
    let profile = body.profile;

    let result = tokio::task::spawn_blocking(move || {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;

        let socket = crate::paths::daemon_socket_for_repo(&repo);
        let mut stream =
            UnixStream::connect(&socket).map_err(|e| format!("Cannot connect to daemon: {}", e))?;
        stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
        stream.set_read_timeout(Some(Duration::from_secs(30))).ok();

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "auth.switch",
            "params": { "profile": profile },
            "id": 1
        });
        writeln!(stream, "{}", request).map_err(|e| format!("Failed to send RPC: {}", e))?;
        stream
            .flush()
            .map_err(|e| format!("Failed to flush: {}", e))?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|e| format!("Failed to read response: {}", e))?;

        let resp: serde_json::Value =
            serde_json::from_str(&line).map_err(|e| format!("Invalid response: {}", e))?;

        if let Some(error) = resp.get("error") {
            let msg = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            return Err(msg.to_string());
        }

        Ok(resp
            .get("result")
            .cloned()
            .unwrap_or(serde_json::json!({"message": "Profile switched"})))
    })
    .await
    .map_err(|e| {
        error!("spawn_blocking panic in auth switch: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({ "error": "Internal server error" })),
        )
    })?;

    match result {
        Ok(data) => Ok(axum::Json(data)),
        Err(msg) => {
            warn!("Auth switch failed: {}", msg);
            Err((
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({ "error": msg })),
            ))
        }
    }
}

/// Get API usage data (session + weekly utilization).
///
/// Fetches from the Anthropic OAuth usage API with a 2-minute TTL cache.
/// Returns 204 No Content if credentials are unavailable or the API call fails.
async fn api_usage() -> Result<impl IntoResponse, StatusCode> {
    let data = tokio::task::spawn_blocking(|| {
        USAGE_CACHE.get(USAGE_CACHE_TTL).or_else(|| {
            let data = crate::usage::fetch_usage_with_credentials()?;
            USAGE_CACHE.set(data.clone());
            Some(data)
        })
    })
    .await
    .map_err(|e| {
        error!("Failed to spawn blocking task for usage: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    match data {
        Some(usage) => Ok(axum::Json(serde_json::json!({
            "session_util": usage.session_util,
            "session_resets": usage.session_resets.to_rfc3339(),
            "week_util": usage.week_util,
            "week_resets": usage.week_resets.to_rfc3339(),
            "account_email": usage.account_email,
        }))),
        None => Err(StatusCode::NO_CONTENT),
    }
}

/// WebSocket upgrade handler
async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<WebState>>) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_websocket(socket, state))
}

/// Handle an individual WebSocket connection
async fn handle_websocket(socket: WebSocket, state: Arc<WebState>) {
    let (mut sender, mut receiver) = socket.split();

    // Assign a unique connection ID for viewer tracking
    let conn_id = {
        let mut tracker = state.viewer_tracker.lock().unwrap();
        tracker.new_conn_id()
    };
    debug!("WebSocket connection {} opened", conn_id);

    // Subscribe to broadcast updates
    let mut updates_rx = state.updates_tx.subscribe();

    // Create a channel for sending error messages back to the client
    let (error_tx, mut error_rx) = tokio::sync::mpsc::channel::<String>(10);

    // Spawn task to forward broadcast updates and error messages to this client
    let send_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                update = updates_rx.recv() => {
                    match update {
                        Ok(update) => {
                            let json = match serde_json::to_string(&update) {
                                Ok(j) => j,
                                Err(e) => {
                                    error!("Failed to serialize update: {}", e);
                                    continue;
                                }
                            };

                            if sender.send(WsMessage::Text(json.into())).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                error_msg = error_rx.recv() => {
                    match error_msg {
                        Some(msg) => {
                            let error_update = WebUpdate::Error(ErrorData { message: msg });
                            let json = match serde_json::to_string(&error_update) {
                                Ok(j) => j,
                                Err(e) => {
                                    error!("Failed to serialize error: {}", e);
                                    continue;
                                }
                            };

                            if sender.send(WsMessage::Text(json.into())).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
        }
    });

    // Handle incoming messages from client
    let state_clone = state.clone();
    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(WsMessage::Text(text)) => {
                if let Err(e) = handle_client_message(&text, &state_clone, conn_id).await {
                    warn!("Error handling client message: {}", e);
                    // Send error back to client using try_send to avoid blocking.
                    // If the channel is full (client is slow), log and drop the error.
                    if let Err(send_err) = error_tx.try_send(e) {
                        warn!(
                            "Failed to send error to WebSocket client (conn {}): {:?}",
                            conn_id, send_err
                        );
                    }
                }
            }
            Ok(WsMessage::Close(_)) => break,
            Err(e) => {
                debug!("WebSocket error: {}", e);
                break;
            }
            _ => {}
        }
    }

    // Clean up viewer tracking on disconnect
    let actions = {
        let mut tracker = state.viewer_tracker.lock().unwrap();
        tracker.remove_viewer(conn_id)
    };
    if !actions.resize_windows.is_empty() || !actions.reset_windows.is_empty() {
        let session = state.viewer_tracker.lock().unwrap().session.clone();
        tokio::task::spawn_blocking(move || actions.execute(&session))
            .await
            .ok();
    }

    send_task.abort();
    debug!("WebSocket connection {} closed", conn_id);
}

/// Handle a message from a WebSocket client
async fn handle_client_message(
    text: &str,
    state: &Arc<WebState>,
    conn_id: u64,
) -> Result<(), String> {
    let msg: ClientMessage =
        serde_json::from_str(text).map_err(|e| format!("Invalid message format: {}", e))?;

    match msg {
        ClientMessage::SendMessage { content } => {
            // Forward to the daemon for processing (handles channel write,
            // WebSocket broadcast, and side-effects like nudging the Lead)
            state
                .channel_post_tx
                .send(MobileChannelPost {
                    content: content.clone(),
                })
                .await
                .map_err(|e| format!("Failed to forward message to daemon: {}", e))?;

            info!("User sent: {}", content);
        }
        ClientMessage::GetHistory => {
            // Client should use the REST endpoint for history
            debug!("Client requested history via WebSocket");
        }
        ClientMessage::GetStatus => {
            // Client should use the REST endpoint for status
            debug!("Client requested status via WebSocket");
        }
        ClientMessage::ViewWindow { window, cols } => {
            // Validate window name
            if window.is_empty()
                || !window
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            {
                return Err("Invalid window name".to_string());
            }
            let actions = {
                let mut tracker = state.viewer_tracker.lock().unwrap();
                tracker.set_viewing(conn_id, window.clone(), cols)
            };
            let session = state.viewer_tracker.lock().unwrap().session.clone();
            debug!(
                "conn {} viewing window {} at {} cols",
                conn_id, window, cols
            );
            tokio::task::spawn_blocking(move || actions.execute(&session))
                .await
                .map_err(|e| format!("Resize task failed: {}", e))?;
        }
        ClientMessage::LeaveWindow => {
            let actions = {
                let mut tracker = state.viewer_tracker.lock().unwrap();
                tracker.stop_viewing(conn_id)
            };
            let session = state.viewer_tracker.lock().unwrap().session.clone();
            debug!("conn {} left window view", conn_id);
            tokio::task::spawn_blocking(move || actions.execute(&session))
                .await
                .map_err(|e| format!("Resize task failed: {}", e))?;
        }
        ClientMessage::Nudge { target, message } => {
            // Validate target name
            if target.is_empty()
                || !target
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            {
                return Err("Invalid nudge target".to_string());
            }
            if message.is_empty() {
                return Err("Empty nudge message".to_string());
            }

            // Web UI only supports nudging the lead, not coworkers
            if target != "lead" {
                return Err(format!(
                    "Cannot nudge coworker {} via web UI - only lead nudges are supported",
                    target
                ));
            }

            let coworkers = state
                .coworkers
                .as_ref()
                .ok_or_else(|| "Coworker manager not available".to_string())?;

            // Use immediate nudge for web UI - user expects instant delivery
            coworkers
                .nudge_lead_immediate(&message)
                .map_err(|e| format!("Failed to nudge lead: {}", e))?;

            info!("Immediate nudge sent to {} via web UI: {}", target, message);
        }
        ClientMessage::SendKey { target, key } => {
            // Validate target name
            if target.is_empty()
                || !target
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            {
                return Err("Invalid send_key target".to_string());
            }
            // Only allow specific safe keys
            if key != "Escape" {
                return Err("Only 'Escape' key is supported".to_string());
            }

            let coworkers = state
                .coworkers
                .as_ref()
                .ok_or_else(|| "Coworker manager not available".to_string())?;

            coworkers
                .send_key(&target, &key)
                .map_err(|e| format!("Failed to send key to {}: {}", target, e))?;
            info!("Sent {} key to {} via web UI", key, target);
        }
    }

    Ok(())
}

/// Create a new broadcast channel for updates
pub fn create_updates_channel() -> (broadcast::Sender<WebUpdate>, broadcast::Receiver<WebUpdate>) {
    broadcast::channel(100)
}

/// Build a `WebUpdate` for a coworker status change.
pub fn coworker_status_update(
    name: &str,
    status: &str,
    current_task: Option<&str>,
    model: &str,
) -> WebUpdate {
    WebUpdate::CoworkerStatus(CoworkerStatusData {
        name: name.to_string(),
        status: status.to_string(),
        current_task: current_task.map(|s| s.to_string()),
        model: model.to_string(),
    })
}

/// Broadcast a coworker status change to all WebSocket clients
pub fn broadcast_coworker_status(
    tx: &broadcast::Sender<WebUpdate>,
    name: &str,
    status: &str,
    current_task: Option<&str>,
    model: &str,
) {
    let _ = tx.send(coworker_status_update(name, status, current_task, model));
}

/// Broadcast lead typing/working status to all WebSocket clients
pub fn broadcast_lead_typing(tx: &broadcast::Sender<WebUpdate>, working: bool) {
    let update = WebUpdate::LeadTyping(LeadTypingData { working });
    let _ = tx.send(update);
}

/// Build a `WebUpdate` for a channel message.
pub fn channel_message_update(message: &Message) -> WebUpdate {
    WebUpdate::ChannelMessage(ChannelMessageData {
        from: message.from.clone(),
        content: message.content.clone(),
        timestamp: message.timestamp.to_rfc3339(),
        msg_type: format!("{:?}", message.message_type).to_lowercase(),
        channel: message.channel_name().to_string(),
    })
}

/// Broadcast a new channel message to all WebSocket clients
pub fn broadcast_channel_message(tx: &broadcast::Sender<WebUpdate>, message: &Message) {
    let _ = tx.send(channel_message_update(message));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_message_parsing() {
        let json = r#"{"type": "send_message", "content": "Hello world"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::SendMessage { content } => {
                assert_eq!(content, "Hello world");
            }
            _ => panic!("Expected SendMessage"),
        }
    }

    #[tokio::test]
    async fn test_mobile_send_message_forwards_to_daemon() {
        // Verify that handle_client_message forwards mobile messages through
        // channel_post_tx instead of writing directly to the channel file.
        let (updates_tx, _) = broadcast::channel(10);
        let (channel_post_tx, mut channel_post_rx) = mpsc::channel(10);

        let state = Arc::new(WebState {
            config: WebConfig::default(),
            updates_tx,
            coworkers: None,
            channel_post_tx,
            push_manager: None,
            all_repo_paths: Vec::new(),
            default_branch: "main".to_string(),
            repo_name_cache: std::sync::RwLock::new(std::collections::HashMap::new()),
            viewer_tracker: Mutex::new(ViewerTracker::new("midtown-test".to_string())),
        });

        let json = r#"{"type": "send_message", "content": "hello from mobile"}"#;
        handle_client_message(json, &state, 1).await.unwrap();

        // The message should be forwarded to the daemon via channel_post_tx
        let post = channel_post_rx
            .try_recv()
            .expect("expected a mobile channel post");
        assert_eq!(post.content, "hello from mobile");
    }

    #[test]
    fn test_web_update_serialization() {
        let update = WebUpdate::ChannelMessage(ChannelMessageData {
            from: "test".to_string(),
            content: "Hello".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            msg_type: "text".to_string(),
            channel: "midtown".to_string(),
        });

        let json = serde_json::to_string(&update).unwrap();
        assert!(json.contains("channel_message"));
        assert!(json.contains("Hello"));
    }

    #[test]
    fn test_coworker_status_update_serialization() {
        let update = WebUpdate::CoworkerStatus(CoworkerStatusData {
            name: "lexington".to_string(),
            status: "running".to_string(),
            current_task: Some("Fix auth bug".to_string()),
        });

        let json = serde_json::to_string(&update).unwrap();
        assert!(json.contains("coworker_status"));
        assert!(json.contains("lexington"));
        assert!(json.contains("running"));
        assert!(json.contains("Fix auth bug"));
    }

    #[test]
    fn test_tmux_pane_query_parsing() {
        // Valid window names
        let json = r#"{"window": "lead"}"#;
        let query: TmuxPaneQuery = serde_json::from_str(json).unwrap();
        assert_eq!(query.window, "lead");

        let json = r#"{"window": "riverside"}"#;
        let query: TmuxPaneQuery = serde_json::from_str(json).unwrap();
        assert_eq!(query.window, "riverside");
    }

    #[test]
    fn test_tmux_window_name_validation() {
        // Valid names: non-empty, alphanumeric, hyphens, underscores
        let valid = |name: &str| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        };

        assert!(valid("lead"));
        assert!(valid("riverside"));
        assert!(valid("my-window"));
        assert!(valid("window_1"));
        assert!(!valid("foo:bar"));
        assert!(!valid("foo;bar"));
        assert!(!valid("foo bar"));
        assert!(!valid(""));
    }

    #[test]
    fn test_lead_typing_update_serialization() {
        let update = WebUpdate::LeadTyping(LeadTypingData { working: true });
        let json = serde_json::to_string(&update).unwrap();
        assert!(json.contains("lead_typing"));
        assert!(json.contains(r#""working":true"#));

        let update = WebUpdate::LeadTyping(LeadTypingData { working: false });
        let json = serde_json::to_string(&update).unwrap();
        assert!(json.contains(r#""working":false"#));
    }

    #[test]
    fn test_client_message_view_window_parsing() {
        let json = r#"{"type": "view_window", "window": "riverside", "cols": 120}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::ViewWindow { window, cols } => {
                assert_eq!(window, "riverside");
                assert_eq!(cols, 120);
            }
            _ => panic!("Expected ViewWindow"),
        }
    }

    #[test]
    fn test_client_message_leave_window_parsing() {
        let json = r#"{"type": "leave_window"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        assert!(matches!(msg, ClientMessage::LeaveWindow));
    }

    #[test]
    fn test_viewer_tracker_single_viewer() {
        let mut tracker = ViewerTracker::new("midtown-test".to_string());
        let conn = tracker.new_conn_id();

        let actions = tracker.set_viewing(conn, "riverside".to_string(), 120);
        assert_eq!(actions.resize_windows.len(), 1);
        assert_eq!(actions.resize_windows[0], ("riverside".to_string(), 120));
        assert!(actions.reset_windows.is_empty());
    }

    #[test]
    fn test_viewer_tracker_max_cols_across_viewers() {
        let mut tracker = ViewerTracker::new("midtown-test".to_string());
        let conn1 = tracker.new_conn_id();
        let conn2 = tracker.new_conn_id();

        tracker.set_viewing(conn1, "riverside".to_string(), 100);
        let actions = tracker.set_viewing(conn2, "riverside".to_string(), 150);

        // Should resize to max(100, 150) = 150
        assert_eq!(actions.resize_windows.len(), 1);
        assert_eq!(actions.resize_windows[0], ("riverside".to_string(), 150));
    }

    #[test]
    fn test_viewer_tracker_remove_recalculates_max() {
        let mut tracker = ViewerTracker::new("midtown-test".to_string());
        let conn1 = tracker.new_conn_id();
        let conn2 = tracker.new_conn_id();

        tracker.set_viewing(conn1, "riverside".to_string(), 100);
        tracker.set_viewing(conn2, "riverside".to_string(), 150);

        // Remove the wider viewer — should resize down to 100
        let actions = tracker.remove_viewer(conn2);
        assert_eq!(actions.resize_windows.len(), 1);
        assert_eq!(actions.resize_windows[0], ("riverside".to_string(), 100));
        assert!(actions.reset_windows.is_empty());
    }

    #[test]
    fn test_viewer_tracker_last_viewer_resets() {
        let mut tracker = ViewerTracker::new("midtown-test".to_string());
        let conn = tracker.new_conn_id();

        tracker.set_viewing(conn, "riverside".to_string(), 120);
        let actions = tracker.remove_viewer(conn);

        // No viewers left — should reset
        assert!(actions.resize_windows.is_empty());
        assert_eq!(actions.reset_windows, vec!["riverside".to_string()]);
    }

    #[test]
    fn test_viewer_tracker_switch_windows() {
        let mut tracker = ViewerTracker::new("midtown-test".to_string());
        let conn = tracker.new_conn_id();

        tracker.set_viewing(conn, "riverside".to_string(), 120);
        let actions = tracker.set_viewing(conn, "park".to_string(), 100);

        // Should reset riverside (no viewers) and resize park
        assert!(actions.reset_windows.contains(&"riverside".to_string()));
        assert!(actions.resize_windows.contains(&("park".to_string(), 100)));
    }

    #[test]
    fn test_viewer_tracker_stop_viewing() {
        let mut tracker = ViewerTracker::new("midtown-test".to_string());
        let conn = tracker.new_conn_id();

        tracker.set_viewing(conn, "riverside".to_string(), 120);
        let actions = tracker.stop_viewing(conn);

        assert_eq!(actions.reset_windows, vec!["riverside".to_string()]);
    }

    #[test]
    fn test_client_message_nudge_parsing() {
        let json = r#"{"type": "nudge", "target": "riverside", "message": "check the tests"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::Nudge { target, message } => {
                assert_eq!(target, "riverside");
                assert_eq!(message, "check the tests");
            }
            _ => panic!("Expected Nudge"),
        }
    }

    #[test]
    fn test_client_message_nudge_lead_parsing() {
        let json = r#"{"type": "nudge", "target": "lead", "message": "please review PR #42"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::Nudge { target, message } => {
                assert_eq!(target, "lead");
                assert_eq!(message, "please review PR #42");
            }
            _ => panic!("Expected Nudge"),
        }
    }

    #[test]
    fn test_client_message_send_key_parsing() {
        let json = r#"{"type": "send_key", "target": "riverside", "key": "Escape"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::SendKey { target, key } => {
                assert_eq!(target, "riverside");
                assert_eq!(key, "Escape");
            }
            _ => panic!("Expected SendKey"),
        }
    }

    #[test]
    fn test_client_message_send_key_lead_parsing() {
        let json = r#"{"type": "send_key", "target": "lead", "key": "Escape"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::SendKey { target, key } => {
                assert_eq!(target, "lead");
                assert_eq!(key, "Escape");
            }
            _ => panic!("Expected SendKey"),
        }
    }

    #[test]
    fn test_coworker_status_update_without_task() {
        let update = WebUpdate::CoworkerStatus(CoworkerStatusData {
            name: "park".to_string(),
            status: "stopped".to_string(),
            current_task: None,
        });

        let json = serde_json::to_string(&update).unwrap();
        assert!(json.contains("coworker_status"));
        assert!(json.contains("park"));
        assert!(json.contains("stopped"));
    }

    #[test]
    fn test_error_update_serialization() {
        let update = WebUpdate::Error(ErrorData {
            message: "Coworker nudge not supported".to_string(),
        });

        let json = serde_json::to_string(&update).unwrap();
        assert!(json.contains("error"));
        assert!(json.contains("Coworker nudge not supported"));
    }

    #[tokio::test]
    async fn test_coworker_nudge_returns_error() {
        // Verify that attempting to nudge a coworker returns an error.
        // Coworker nudges are not supported via the web UI - only lead nudges are allowed.
        let (updates_tx, _) = broadcast::channel(10);
        let (channel_post_tx, _) = mpsc::channel(10);

        let state = Arc::new(WebState {
            config: WebConfig::default(),
            updates_tx,
            coworkers: None, // No coworker manager available
            channel_post_tx,
            push_manager: None,
            all_repo_paths: Vec::new(),
            default_branch: "main".to_string(),
            repo_name_cache: std::sync::RwLock::new(std::collections::HashMap::new()),
            viewer_tracker: Mutex::new(ViewerTracker::new("midtown-test".to_string())),
        });

        let json = r#"{"type": "nudge", "target": "lexington", "message": "test nudge"}"#;
        let result = handle_client_message(json, &state, 1).await;

        // Should return an error since coworker nudges are not supported via web UI
        assert!(result.is_err());
        let err_msg = result.unwrap_err();
        assert!(err_msg.contains("Cannot nudge coworker"));
        assert!(err_msg.contains("lexington"));
    }

    #[tokio::test]
    async fn test_coworker_nudge_not_supported_via_web_ui() {
        // Verify that nudging a coworker (not lead) returns "not supported via web UI"
        use crate::coworker::CoworkerManager;
        use crate::worktree::WorktreeManager;
        use tempfile::TempDir;

        let (updates_tx, _) = broadcast::channel(10);
        let (channel_post_tx, _) = mpsc::channel(10);

        // Create a minimal CoworkerManager for testing
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(temp_dir.path())
            .output()
            .expect("Failed to init git repo");
        std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(temp_dir.path())
            .output()
            .ok();
        std::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(temp_dir.path())
            .output()
            .ok();
        std::process::Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(temp_dir.path())
            .output()
            .ok();

        let worktree_manager = WorktreeManager::new(temp_dir.path().to_path_buf())
            .expect("Failed to create worktree manager");
        let coworkers = CoworkerManager::new("midtown-test", worktree_manager);

        let state = Arc::new(WebState {
            config: WebConfig::default(),
            updates_tx,
            coworkers: Some(coworkers),
            channel_post_tx,
            push_manager: None,
            all_repo_paths: Vec::new(),
            default_branch: "main".to_string(),
            repo_name_cache: std::sync::RwLock::new(std::collections::HashMap::new()),
            viewer_tracker: Mutex::new(ViewerTracker::new("midtown-test".to_string())),
        });

        // Try to nudge a coworker (not "lead")
        let json = r#"{"type": "nudge", "target": "lexington", "message": "test nudge"}"#;
        let result = handle_client_message(json, &state, 1).await;

        // Should return the "Cannot nudge coworker" error
        assert!(result.is_err());
        let err_msg = result.unwrap_err();
        assert!(err_msg.contains("Cannot nudge coworker"));
        assert!(err_msg.contains("lexington"));
    }

    #[tokio::test]
    async fn test_error_channel_backpressure() {
        // Stress test: verify that error channel backpressure doesn't block message handling.
        // Generate 20 errors rapidly (channel capacity is 10) and ensure the handler continues.
        use crate::coworker::CoworkerManager;
        use crate::worktree::WorktreeManager;
        use tempfile::TempDir;

        let (updates_tx, _) = broadcast::channel(10);
        let (channel_post_tx, _) = mpsc::channel(10);

        // Create a minimal CoworkerManager
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(temp_dir.path())
            .output()
            .ok();
        std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(temp_dir.path())
            .output()
            .ok();
        std::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(temp_dir.path())
            .output()
            .ok();
        std::process::Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(temp_dir.path())
            .output()
            .ok();

        let worktree_manager = WorktreeManager::new(temp_dir.path().to_path_buf())
            .expect("Failed to create worktree manager");
        let coworkers = CoworkerManager::new("midtown-test", worktree_manager);

        let state = Arc::new(WebState {
            config: WebConfig::default(),
            updates_tx,
            coworkers: Some(coworkers),
            channel_post_tx,
            push_manager: None,
            all_repo_paths: Vec::new(),
            default_branch: "main".to_string(),
            repo_name_cache: std::sync::RwLock::new(std::collections::HashMap::new()),
            viewer_tracker: Mutex::new(ViewerTracker::new("midtown-test".to_string())),
        });

        // Trigger 20 errors (channel capacity is 10) by sending invalid messages.
        // The key assertion: handle_client_message should not hang or panic.
        for i in 0..20 {
            let json = format!(r#"{{"type": "invalid_type_{}"}}"#, i);
            let result = handle_client_message(&json, &state, 1).await;
            // All should error (invalid message type)
            assert!(result.is_err(), "Expected error for invalid message {}", i);
        }

        // If we reach here without hanging, backpressure is handled correctly
    }
}
