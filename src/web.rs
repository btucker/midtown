//! Web API server for Svelte mobile app
//!
//! Provides API endpoints and WebSocket connections for live updates
//! (channel messages, coworker status, etc.). Static files are served
//! only by the shared gateway on port 47022.

use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Multipart, Path, Query, State,
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

/// Thread-safe TTL cache that also validates a key on lookup.
///
/// Returns cached data only when both the TTL is valid AND the key matches.
/// Used for caches where the input parameters can change between requests
/// (e.g., the set of active profiles for usage data).
struct KeyedTtlCache<K, V> {
    inner: Mutex<Option<(Instant, K, V)>>,
}

impl<K: Clone + PartialEq, V: Clone> KeyedTtlCache<K, V> {
    const fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    /// Return cached value if it exists, is younger than `ttl`, and the key matches.
    fn get(&self, ttl: Duration, key: &K) -> Option<V> {
        let guard = self.inner.lock().ok()?;
        guard
            .as_ref()
            .filter(|(ts, k, _)| ts.elapsed() < ttl && k == key)
            .map(|(_, _, v)| v.clone())
    }

    /// Store a new value with its key and the current timestamp.
    fn set(&self, key: K, value: V) {
        if let Ok(mut guard) = self.inner.lock() {
            *guard = Some((Instant::now(), key, value));
        }
    }
}

/// Cached repo status (commit, CI, release).
static REPO_STATUS_CACHE: TtlCache<RepoStatus> = TtlCache::new();

/// Cached open PR list.
static OPEN_PRS_CACHE: TtlCache<Vec<serde_json::Value>> = TtlCache::new();

/// Cached merged PR list.
static MERGED_PRS_CACHE: TtlCache<Vec<serde_json::Value>> = TtlCache::new();

/// Cached multi-account usage data, keyed by the active profile set.
///
/// The cache key is a sorted, serialized representation of the provider/profile
/// combinations being fetched. When coworkers spawn or shut down, the profile set
/// changes and the cache misses naturally, avoiding stale data.
static MULTI_USAGE_CACHE: KeyedTtlCache<String, Vec<crate::usage::UsageData>> =
    KeyedTtlCache::new();

/// TTL for usage data cache (2 minutes, matching TUI refresh interval).
const USAGE_CACHE_TTL: Duration = Duration::from_secs(120);

/// TTL for repo status cache (commit hash, CI status, latest release).
///
/// These values change infrequently: commits land every few minutes at most,
/// CI runs take minutes to complete, and releases are rare. Using 30s (same as
/// PR cache) causes 3 unnecessary REST API calls per 30s poll cycle.
/// 5 minutes strikes a balance between freshness and API conservation.
const REPO_STATUS_CACHE_TTL: Duration = Duration::from_secs(300);

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
    pub channel: Option<String>,
    pub thread_parent_id: Option<String>,
}

/// Thread ownership info returned by the thread_ownership API.
#[derive(Debug, Clone, Serialize)]
pub struct ThreadOwnershipInfo {
    /// Display name of the session that owns this thread (channel lead or fork name).
    pub owner: String,
    /// True when the thread is handled by a dedicated fork session.
    pub is_fork: bool,
    /// Channel name this thread belongs to.
    pub channel: Option<String>,
}

/// A request from the web server to the daemon that expects a response.
pub enum WebRpcRequest {
    /// Fork a thread: create a dedicated session for the given thread.
    ForkThread {
        thread_parent_id: String,
        channel_name: String,
        response_tx: tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>,
    },
    /// Unfork a thread: kill the dedicated session and return to channel lead.
    UnforkThread {
        thread_parent_id: String,
        response_tx: tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>,
    },
    /// Query thread ownership (who handles this thread).
    ThreadOwnership {
        thread_parent_id: String,
        channel_name: String,
        response_tx: tokio::sync::oneshot::Sender<Result<ThreadOwnershipInfo, String>>,
    },
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
    /// Maximum number of coworkers that can be spawned
    pub max_coworkers: usize,
    /// Cached GitHub repo full names (owner/repo) by repo path.
    /// Repo names never change during a session, so we cache indefinitely.
    pub repo_name_cache: std::sync::RwLock<std::collections::HashMap<std::path::PathBuf, String>>,
    /// Sender for thread fork/unfork/ownership RPCs to the daemon.
    /// Optional because standalone webserver tests don't need the daemon.
    pub web_rpc_tx: Option<mpsc::Sender<WebRpcRequest>>,
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
    /// Error response for a client action
    #[serde(rename = "error")]
    Error(ErrorData),
    /// Universal event items from agent sessions
    #[serde(rename = "universal_items")]
    UniversalItems(UniversalItemsData),
    /// A coworker is waiting for user input (AskUserQuestion tool call)
    #[serde(rename = "coworker_question")]
    CoworkerQuestion(CoworkerQuestionData),
    /// Channel list changed (create, archive, unarchive, rename)
    #[serde(rename = "channel_list_changed")]
    ChannelListChanged(ChannelListChangedData),
}

#[derive(Debug, Clone, Serialize)]
pub struct ThreadReplySummary {
    pub from: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelMessageData {
    /// Unique message identifier
    pub id: String,
    pub from: String,
    pub content: String,
    pub timestamp: String,
    #[serde(rename = "type")]
    pub msg_type: String,
    pub channel: String,
    /// Optional thread parent message ID. When set, this message is a reply
    /// in a thread started by the message with this ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_parent_id: Option<String>,
    /// Number of replies in this message's thread (top-level history only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_count: Option<usize>,
    /// Last reply metadata for this message's thread (top-level history only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_reply: Option<ThreadReplySummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoworkerStatusData {
    pub name: String,
    /// Omitted from progress-only updates to avoid overwriting the status
    /// (e.g., "completed") set by a spawn/status update in the frontend.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Omitted from progress-only updates to avoid clobbering the task name
    /// stored in the frontend (frontend merges via shallow spread).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_task: Option<String>,
    /// Model name. Omitted from progress-only updates to avoid overwriting
    /// the model previously set by a spawn/status update.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Claude session ID for this coworker session, if known.
    /// Enables the web UI to distinguish between multiple sessions
    /// that share the same coworker name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Workflow phase abbreviation (e.g., "dev", "test", "PR").
    /// Present when coworker has reported a phase via `midtown state`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Progress percentage (0-100) reported by the coworker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    /// Human-readable estimated time remaining (e.g., "~3m", "~30s").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_estimate: Option<String>,
    /// Health status color: "green", "yellow", or "red".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorData {
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UniversalItemsData {
    pub agent_name: String,
    /// The channel this agent's tool calls belong to. `None` for the main lead (main channel),
    /// `Some(channel_name)` for a channel lead (displayed only in that topic channel).
    pub channel: Option<String>,
    /// When set, this agent's tool calls should appear in the thread panel for this
    /// thread parent ID rather than in the main channel activity strip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_parent_id: Option<String>,
    pub items: Vec<crate::universal_events::UniversalItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoworkerQuestionData {
    pub id: u64,
    pub coworker_name: String,
    pub question: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelListChangedData {
    /// What happened: "created", "archived", "unarchived", "renamed"
    pub action: String,
    /// Channel name affected
    pub channel: String,
}

/// WebSocket message from client
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    /// Send a message to the channel (to lead)
    #[serde(rename = "send_message")]
    SendMessage {
        content: String,
        #[serde(default)]
        channel: Option<String>,
        #[serde(default)]
        thread_parent_id: Option<String>,
    },
    /// Request full channel history
    #[serde(rename = "get_history")]
    GetHistory,
    /// Request coworker status
    #[serde(rename = "get_status")]
    GetStatus,
    /// Send a nudge (text input) to a coworker or the lead
    #[serde(rename = "nudge")]
    Nudge { target: String, message: String },
    /// Send a special key (like Escape) to a coworker or the lead
    #[serde(rename = "send_key")]
    SendKey { target: String, key: String },
    /// Answer a coworker's pending question
    #[serde(rename = "answer_question")]
    AnswerQuestion {
        coworker_name: String,
        answer: String,
    },
}

/// Create the web server router
///
/// This can be nested into the main webhook server.
/// Only serves API endpoints — static files are handled by the shared gateway.
pub fn create_web_router(state: Arc<WebState>) -> Router {
    Router::new()
        .route("/api/ws", get(ws_handler))
        .route("/api/health", get(api_health))
        .route("/api/channels/history", get(api_channel_history))
        .route("/api/channels", get(api_channels_list))
        .route("/api/channels/create", post(api_channels_create))
        .route("/api/status", get(api_status))
        .route("/api/zellij-web-url", get(api_zellij_web_url))
        .route("/api/push/vapid-key", get(api_push_vapid_key))
        .route("/api/push/subscribe", post(api_push_subscribe))
        .route("/api/push/unsubscribe", post(api_push_unsubscribe))
        .route("/api/auth/profiles", get(api_auth_profiles))
        .route("/api/auth/login", post(api_auth_login))
        .route("/api/auth/switch", post(api_auth_switch))
        .route("/api/auth/pool-toggle", post(api_auth_pool_toggle))
        .route("/api/usage", get(api_usage))
        .route("/api/questions", get(api_pending_questions))
        .route("/api/search", get(api_search))
        .route("/api/upload", post(api_upload))
        .route("/api/uploads/{filename}", get(api_get_upload))
        .route("/api/threads/fork", post(api_thread_fork))
        .route("/api/threads/unfork", post(api_thread_unfork))
        .route("/api/threads/ownership", get(api_thread_ownership))
        .layer(DefaultBodyLimit::max(11 * 1024 * 1024))
        .with_state(state)
}

/// Health check endpoint
async fn api_health() -> &'static str {
    "ok"
}

/// Query parameters for channel listing
#[derive(Debug, Deserialize)]
struct ChannelListQuery {
    /// Include archived channels in the list (default: false)
    #[serde(default)]
    include_archived: bool,
}

/// List all available channels for the current repository
async fn api_channels_list(
    State(state): State<Arc<WebState>>,
    Query(query): Query<ChannelListQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let base_dir = crate::paths::projects_dir_for_repo(&state.config.repo);
    let channels = Channel::list(base_dir, query.include_archived, Some(&state.config.repo))
        .map_err(|e| {
            error!("Failed to list channels: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(axum::Json(serde_json::json!({ "channels": channels })))
}

/// Request body for channel creation
#[derive(Debug, Deserialize)]
struct CreateChannelRequest {
    name: String,
}

/// Validate a channel name for read operations (history, thread fetching).
///
/// Accepts any non-empty name containing only alphanumeric characters, hyphens, and
/// underscores. This includes "midtown", which is a valid channel to read from.
fn validate_channel_name_for_history(
    name: &str,
) -> Result<(), (StatusCode, axum::Json<serde_json::Value>)> {
    if name.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({ "error": "Channel name cannot be empty" })),
        ));
    }

    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err((
            StatusCode::BAD_REQUEST,
            axum::Json(
                serde_json::json!({ "error": "Channel name must contain only alphanumeric characters, hyphens, and underscores" }),
            ),
        ));
    }

    Ok(())
}

/// Validate a channel name for write/creation operations.
///
/// Channel names must:
/// - Be non-empty
/// - Contain only alphanumeric characters, hyphens, and underscores
/// - Not be the main channel name (reserved for the project's default channel)
///
/// DM channels (`dm-<coworker>`) are explicitly allowed. The coworker suffix must
/// be non-empty and contain only alphanumeric characters and hyphens.
fn validate_channel_name(
    name: &str,
    main_channel_name: &str,
) -> Result<(), (StatusCode, axum::Json<serde_json::Value>)> {
    validate_channel_name_for_history(name)?;

    // DM channels (dm-<coworker>) are allowed — validate the coworker suffix.
    if let Some(coworker) = name.strip_prefix("dm-") {
        if coworker.is_empty() || !coworker.chars().all(|c| c.is_alphanumeric() || c == '-') {
            return Err((
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({
                    "error": "DM channel name must be 'dm-<coworker>' where coworker contains only alphanumeric characters and hyphens"
                })),
            ));
        }
        return Ok(());
    }

    if name == main_channel_name {
        return Err((
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({
                "error": format!("Cannot use reserved channel name '{}'", main_channel_name)
            })),
        ));
    }

    Ok(())
}

/// Create a new channel.
///
/// Accepts a POST request with JSON body `{"name": "channel-name"}`.
/// Returns 201 Created on success, 400 Bad Request if the name is invalid.
async fn api_channels_create(
    State(state): State<Arc<WebState>>,
    Json(body): Json<CreateChannelRequest>,
) -> Result<impl IntoResponse, (StatusCode, axum::Json<serde_json::Value>)> {
    let channel_name = body.name.trim();

    validate_channel_name(channel_name, &state.config.repo)?;

    // Create the channel (idempotent - returns existing channel if it already exists)
    let base_dir = crate::paths::projects_dir_for_repo(&state.config.repo);
    let already_exists = base_dir.join("channels").join(channel_name).exists();
    Channel::create(base_dir, channel_name).map_err(|e| {
        error!("Failed to create channel '{}': {}", channel_name, e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({ "error": "Failed to create channel" })),
        )
    })?;

    info!("Created channel '{}'", channel_name);
    if !already_exists {
        let _ = state
            .updates_tx
            .send(channel_list_changed("created", channel_name));
    }
    Ok((
        StatusCode::CREATED,
        axum::Json(serde_json::json!({ "name": channel_name })),
    ))
}

/// Returns true if a message belongs to the given thread: either it is the
/// parent message itself or a reply to it. Used by `api_channel_history` and
/// tested directly in unit tests.
pub(crate) fn is_in_thread(msg: &crate::message::Message, parent_id: &str) -> bool {
    msg.id == parent_id || msg.thread_parent_id.as_deref() == Some(parent_id)
}

/// Query parameters for channel history
#[derive(Debug, Deserialize)]
struct ChannelHistoryQuery {
    /// Optional channel name to filter by. If not provided, returns all messages from the main channel.
    channel: Option<String>,
    /// Optional thread parent ID to filter by. If provided, return the parent
    /// message itself plus all replies to it.
    thread_parent_id: Option<String>,
    /// Maximum number of messages to return (default: 500). Only applies to
    /// non-thread queries; thread queries always return all replies.
    limit: Option<usize>,
}

/// Get channel message history
///
/// Accepts an optional `?channel=name` query parameter to load a specific channel.
/// If `thread_parent_id` is omitted, returns top-level messages only (with thread
/// reply metadata). If `thread_parent_id` is provided, returns the parent message
/// plus all replies in that thread.
async fn api_channel_history(
    State(state): State<Arc<WebState>>,
    Query(params): Query<ChannelHistoryQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let channel = if let Some(ref channel_name) = params.channel {
        // Validate channel name to prevent path traversal (allow "midtown" for reads)
        validate_channel_name_for_history(channel_name).map_err(|_| StatusCode::BAD_REQUEST)?;

        // Load a specific channel by name
        Channel::for_repo_named(&state.config.repo, channel_name).map_err(|e| {
            error!("Failed to open channel '{}': {}", channel_name, e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?
    } else {
        // Default: load the main channel
        Channel::for_repo(&state.config.repo).map_err(|e| {
            error!("Failed to open channel: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?
    };

    let response: Vec<ChannelMessageData> = match params.thread_parent_id {
        // Thread history query: read ALL messages so we find every reply regardless
        // of how far back it was posted. Thread responses are small, so this is safe.
        Some(parent_id) => {
            let messages = channel.read_all_async().await.map_err(|e| {
                error!("Failed to read channel: {}", e);
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
            messages
                .into_iter()
                .filter(|m| is_in_thread(m, &parent_id))
                .map(|m| {
                    let channel = m.channel_name().to_string();
                    ChannelMessageData {
                        id: m.id.clone(),
                        from: m.from,
                        content: m.content,
                        timestamp: m.timestamp.to_rfc3339(),
                        msg_type: format!("{:?}", m.message_type).to_lowercase(),
                        channel,
                        thread_parent_id: m.thread_parent_id,
                        reply_count: None,
                        last_reply: None,
                    }
                })
                .collect()
        }
        // Default history query: load only the most recent N messages, then
        // return top-level messages annotated with thread reply metadata.
        None => {
            let limit = params.limit.unwrap_or(500);
            let (messages, _count) =
                channel
                    .read_last_n_messages_async(limit)
                    .await
                    .map_err(|e| {
                        error!("Failed to read channel: {}", e);
                        StatusCode::INTERNAL_SERVER_ERROR
                    })?;

            let mut reply_meta: std::collections::HashMap<String, (usize, ThreadReplySummary)> =
                std::collections::HashMap::new();
            for msg in &messages {
                if let Some(parent_id) = msg.thread_parent_id.as_ref() {
                    let entry = reply_meta.entry(parent_id.clone()).or_insert((
                        0,
                        ThreadReplySummary {
                            from: msg.from.clone(),
                            timestamp: msg.timestamp.to_rfc3339(),
                        },
                    ));
                    entry.0 += 1;
                    entry.1 = ThreadReplySummary {
                        from: msg.from.clone(),
                        timestamp: msg.timestamp.to_rfc3339(),
                    };
                }
            }

            messages
                .into_iter()
                .filter(|m| m.thread_parent_id.is_none())
                .map(|m| {
                    let channel = m.channel_name().to_string();
                    let (reply_count, last_reply) = match reply_meta.get(&m.id) {
                        Some((count, last)) => (Some(*count), Some(last.clone())),
                        None => (None, None),
                    };
                    ChannelMessageData {
                        id: m.id.clone(),
                        from: m.from,
                        content: m.content,
                        timestamp: m.timestamp.to_rfc3339(),
                        msg_type: format!("{:?}", m.message_type).to_lowercase(),
                        channel,
                        thread_parent_id: m.thread_parent_id,
                        reply_count,
                        last_reply,
                    }
                })
                .collect()
        }
    };

    Ok(axum::Json(response))
}

/// Query parameters for full-text search
#[derive(Debug, Deserialize)]
struct SearchQuery {
    /// Search query string
    q: String,
    /// Maximum number of results (default: 50)
    #[serde(default = "default_search_limit")]
    limit: usize,
}

fn default_search_limit() -> usize {
    50
}

/// Full-text search across all channel message history
async fn api_search(
    State(state): State<Arc<WebState>>,
    Query(params): Query<SearchQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let project_dir = crate::paths::projects_dir_for_repo(&state.config.repo);

    let response = crate::search::search_messages(&project_dir, &params.q, params.limit)
        .await
        .map_err(|e| {
            error!("Search failed: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

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

/// Send a single JSON-RPC request to the daemon and return the `result` field.
///
/// Connects to the daemon's Unix socket, sends the request, and reads one
/// response line. Returns `None` if the daemon is unreachable or the response
/// is malformed.
fn daemon_rpc(repo: &str, method: &str) -> Option<serde_json::Value> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let socket = crate::paths::daemon_socket_for_repo(repo);
    let mut stream = UnixStream::connect(&socket).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "id": 1
    });
    writeln!(stream, "{}", request).ok()?;
    stream.flush().ok()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;

    let resp: serde_json::Value = serde_json::from_str(&line).ok()?;
    resp.get("result").cloned()
}

/// Fetch PR data (open PRs + recently merged) from the daemon via `prs.status` RPC.
///
/// Returns `None` if the daemon is unreachable or the response is unexpected.
fn fetch_prs_via_rpc(repo: &str) -> Option<(Vec<serde_json::Value>, Vec<serde_json::Value>)> {
    let result = daemon_rpc(repo, "prs.status")?;
    let prs = result.get("prs")?.as_array()?.clone();
    let merged = result.get("merged_prs")?.as_array()?.clone();
    Some((prs, merged))
}

/// Fetch live coworker state from the daemon via `coworkers.status` RPC.
///
/// Returns an empty vec if the daemon is unreachable.
fn fetch_coworkers_via_rpc(repo: &str) -> Vec<serde_json::Value> {
    daemon_rpc(repo, "coworkers.status")
        .as_ref()
        .and_then(|r| r.get("coworkers"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
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
/// PR data comes from the daemon's `prs.status` RPC (60s server-side cache).
/// Coworker state comes from `coworkers.status` (live, no cache).
/// Both fall back to cached gh CLI calls if the daemon is unreachable.
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
                "channel": task.channel,
                "blocked_by": task.blocked_by,
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
    // Channel lead names for filtering (channel leads must not appear in coworker status)
    let channel_lead_names: std::collections::HashSet<String> = persistent_state
        .channel_lead_sessions
        .keys()
        .cloned()
        .collect();

    // --- PR data: prefer prs.status RPC (60s server-side cache), fall back to gh CLI ---
    // --- Coworker data: prefer coworkers.status RPC (live, no cache) ---
    let repo_name = state.config.repo.clone();
    let (pull_requests, merged_prs, rpc_coworkers) = tokio::task::spawn_blocking(move || {
        // Fetch PR data from daemon (cached 60s server-side)
        let (rpc_prs, rpc_merged) = fetch_prs_via_rpc(&repo_name).unwrap_or_else(|| {
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
        });

        // Fetch coworker state separately (live, no cache)
        let rpc_coworkers = fetch_coworkers_via_rpc(&repo_name);
        let coworkers = if rpc_coworkers.is_empty() {
            None
        } else {
            Some(rpc_coworkers)
        };

        (rpc_prs, rpc_merged, coworkers)
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

    // Use coworker data from RPC if available (includes task_id, phase, pr_number, health),
    // otherwise fall back to basic coworker data from CoworkerManager
    let coworkers_data: Vec<serde_json::Value> = if let Some(rpc_coworkers) = rpc_coworkers {
        // RPC data already has all the fields we need
        rpc_coworkers
    } else {
        // Fall back to CoworkerManager data, transforming to match RPC structure
        // Build map of owner -> (task_id, subject) for active tasks
        let coworker_tasks: std::collections::HashMap<String, (Option<u32>, String)> = tasks
            .iter()
            .filter_map(|t| {
                let status = t.get("status").and_then(|s| s.as_str()).unwrap_or("");
                let owner = t.get("owner").and_then(|o| o.as_str()).unwrap_or("");
                let subject = t.get("subject").and_then(|s| s.as_str()).unwrap_or("");
                let task_id = t
                    .get("id")
                    .and_then(|id| id.as_str())
                    .and_then(|s| s.parse::<u32>().ok());
                if status == "in_progress" && !owner.is_empty() {
                    Some((owner.to_lowercase(), (task_id, subject.to_string())))
                } else {
                    None
                }
            })
            .collect();

        // Build reverse map: PR number -> source task ID (from PR titles)
        // This ensures reviewers show the meaningful task ID, not their ephemeral internal ID
        let task_id_by_pr: std::collections::HashMap<u64, u32> = pull_requests
            .iter()
            .filter_map(|pr| {
                let pr_number = pr.get("number")?.as_u64()?;
                let title = pr.get("title")?.as_str()?;
                let task_id = extract_task_id_from_pr_title(title)?;
                // extract_task_id_from_pr_title returns u64, but task IDs are stored as u32
                // Use try_from to safely convert, skipping entries that overflow u32::MAX
                let task_id_u32 = u32::try_from(task_id).ok()?;
                Some((pr_number, task_id_u32))
            })
            .collect();

        state
            .coworkers
            .as_ref()
            .map(|mgr| {
                mgr.list()
                    .into_iter()
                    .filter_map(|cw| {
                        // Skip channel lead sessions and the lead itself — they are
                        // not regular dev/reviewer coworkers and must not appear in
                        // the general coworker status panel.
                        if channel_lead_names.contains(&cw.name)
                            || cw.name.eq_ignore_ascii_case("lead")
                        {
                            return None;
                        }
                        // Skip idle/stopped coworkers (matching daemon RPC logic)
                        if cw.status.to_string() == "stopped" {
                            return None;
                        }

                        // Look up current task from task storage (case-insensitive)
                        let (internal_task_id, pr_number) = if let Some((tid, _)) =
                            coworker_tasks.get(&cw.name.to_lowercase())
                        {
                            // Has a task — try to find associated PR number
                            let pr_num = tid.and_then(|id| {
                                pull_requests
                                    .iter()
                                    .find(|pr| {
                                        pr.get("task_id").and_then(|v| v.as_u64()).map(|v| v as u32)
                                            == Some(id)
                                    })
                                    .and_then(|pr| pr.get("number").and_then(|n| n.as_u64()))
                            });
                            (*tid, pr_num)
                        } else if let Some(assignment) =
                            persistent_state.worktree_registry.get_by_coworker(&cw.name)
                        {
                            // No task in storage, but has a worktree (reviewing or PR handoff)
                            // Parse task_id from String to u32
                            let task_id_u32 = assignment
                                .task_id
                                .as_ref()
                                .and_then(|s| s.parse::<u32>().ok());
                            (task_id_u32, assignment.pr_number)
                        } else {
                            (None, None)
                        };

                        // For display: prefer source task ID (from PR title) over internal task ID
                        // This ensures reviewers show the meaningful task ID, not their ephemeral one
                        let display_task_id = pr_number
                            .and_then(|pr| task_id_by_pr.get(&pr).copied())
                            .or(internal_task_id);

                        // Derive phase from status (best-effort — daemon has more detail)
                        // Fallback doesn't have WorkflowPhase, so use a simple heuristic
                        let phase = if pr_number.is_some() {
                            Some("PR") // Has a PR, likely in PR phase
                        } else if display_task_id.is_some() {
                            Some("dev") // Has a task but no PR, likely developing
                        } else {
                            None
                        };

                        // Health defaults to green (fallback can't access HeadlessHealth)
                        let health = "green";

                        Some(serde_json::json!({
                            "name": cw.name,
                            "task_id": display_task_id,
                            "phase": phase,
                            "pr_number": pr_number,
                            "health": health,
                        }))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };

    // Fetch repo status with TTL cache (blocking I/O).
    // Uses a longer TTL than PR data: commits/CI/releases change infrequently.
    let default_branch = state.default_branch.clone();
    let repo_status = tokio::task::spawn_blocking(move || {
        REPO_STATUS_CACHE
            .get(REPO_STATUS_CACHE_TTL)
            .unwrap_or_else(|| {
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
        "max_coworkers": state.max_coworkers,
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
/// Get the Zellij web client URL for embedding in the web app.
///
/// Returns the URL of the Zellij web client and the session name,
/// so the Svelte app can embed it in an iframe.
async fn api_zellij_web_url(State(state): State<Arc<WebState>>) -> impl IntoResponse {
    let session = format!("midtown-{}", state.config.repo);
    Json(serde_json::json!({
        "url": "https://localhost:6780",
        "session": session,
    }))
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
#[derive(Debug, Deserialize, Default)]
struct AuthProfilesQuery {
    provider: Option<String>,
}

async fn api_auth_profiles(
    Query(query): Query<AuthProfilesQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let provider = query
        .provider
        .as_deref()
        .map(str::parse::<crate::auth::AuthProvider>)
        .transpose()
        .map_err(|_| StatusCode::BAD_REQUEST)?
        .unwrap_or_default();

    let profiles = tokio::task::spawn_blocking(move || {
        // Use project-aware profile resolution so "active" reflects the current project
        let project = crate::paths::detect_repo_name().unwrap_or_default();
        let current = if project.is_empty() {
            crate::auth::current_profile_for(provider)
        } else {
            crate::auth::active_profile_for_project_with_provider(&project, provider)
        };
        let names = crate::auth::list_profiles_for(provider).unwrap_or_default();
        names
            .into_iter()
            .map(|name| {
                let status = crate::auth::profile_status_for(provider, &name);
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

/// Request body for starting OAuth login.
#[derive(Debug, Deserialize)]
struct AuthLoginRequest {
    email: String,
    /// Provider to log in with ("claude" or "codex"). Defaults to "claude".
    provider: Option<String>,
}

/// Start an OAuth login flow.
///
/// Spawns the provider CLI which opens the default browser for OAuth. The CLI
/// handles the full flow autonomously (browser open → authenticate → callback).
async fn api_auth_login(
    Json(body): Json<AuthLoginRequest>,
) -> Result<impl IntoResponse, (StatusCode, axum::Json<serde_json::Value>)> {
    // Validate email
    if !body.email.contains('@') {
        return Err((
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({ "error": "Invalid email address" })),
        ));
    }

    let provider = body
        .provider
        .as_deref()
        .map(str::parse::<crate::auth::AuthProvider>)
        .transpose()
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({ "error": e })),
            )
        })?
        .unwrap_or_default();

    let email = body.email;

    let result =
        tokio::task::spawn_blocking(move || crate::auth::start_login(provider, &email, false))
            .await
            .map_err(|e| {
                error!("spawn_blocking panic in auth login: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    axum::Json(serde_json::json!({ "error": "Internal server error" })),
                )
            })?;

    match result {
        Ok(_child) => {
            // Child is dropped — the CLI process runs detached in the background,
            // opens the browser, handles the OAuth callback, and exits on its own.
            Ok(axum::Json(
                serde_json::json!({ "message": "Login started — check your browser" }),
            ))
        }
        Err(msg) => {
            warn!("Auth login failed: {}", msg);
            Err((
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({ "error": msg })),
            ))
        }
    }
}

/// Request body for switching auth profiles.
#[derive(Debug, Deserialize)]
struct AuthSwitchRequest {
    profile: String,
    /// When true, switch globally for all projects. Default: current project only.
    #[serde(default)]
    all: bool,
    /// Provider to switch for ("claude" or "codex"). Defaults to "claude".
    provider: Option<String>,
}

/// Switch the active auth profile via the daemon's RPC.
///
/// Proxies to the daemon's `auth.switch` RPC, which shuts down all coworkers,
/// switches the profile on disk, and (for Claude) re-launches the lead.
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
    let provider = body
        .provider
        .as_deref()
        .map(str::parse::<crate::auth::AuthProvider>)
        .transpose()
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({ "error": e })),
            )
        })?
        .unwrap_or_default();

    let repo = state.config.repo.clone();
    let profile = body.profile;
    let all = body.all;

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
            "params": { "profile": profile, "all": all, "provider": provider.as_str() },
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

/// Request body for toggling an auth profile in/out of the coworker pool.
#[derive(Debug, Deserialize)]
struct AuthPoolToggleRequest {
    profile: String,
    /// Whether to add (`true`) or remove (`false`) from the pool.
    enabled: bool,
    /// Provider ("claude" or "codex"). Defaults to "claude".
    provider: Option<String>,
}

/// Toggle whether an auth profile is in the coworker spawn pool.
///
/// Proxies to the daemon's `auth.pool-toggle` RPC, which modifies the
/// `execution.coworker_profiles` list in the project config.
async fn api_auth_pool_toggle(
    State(state): State<Arc<WebState>>,
    Json(body): Json<AuthPoolToggleRequest>,
) -> Result<impl IntoResponse, (StatusCode, axum::Json<serde_json::Value>)> {
    if let Err(e) = crate::auth::validate_profile_name(&body.profile) {
        return Err((
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({ "error": e.to_string() })),
        ));
    }
    let provider = body
        .provider
        .as_deref()
        .map(str::parse::<crate::auth::AuthProvider>)
        .transpose()
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({ "error": e })),
            )
        })?
        .unwrap_or_default();

    let repo = state.config.repo.clone();
    let profile = body.profile;
    let enabled = body.enabled;

    let result = tokio::task::spawn_blocking(move || {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;

        let socket = crate::paths::daemon_socket_for_repo(&repo);
        let mut stream =
            UnixStream::connect(&socket).map_err(|e| format!("Cannot connect to daemon: {}", e))?;
        stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
        stream.set_read_timeout(Some(Duration::from_secs(10))).ok();

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "auth.pool-toggle",
            "params": { "profile": profile, "provider": provider.as_str(), "enabled": enabled },
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
            .unwrap_or(serde_json::json!({"success": true})))
    })
    .await
    .map_err(|e| {
        error!("spawn_blocking panic in pool toggle: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({ "error": "Internal server error" })),
        )
    })?;

    match result {
        Ok(data) => Ok(axum::Json(data)),
        Err(msg) => {
            warn!("Pool toggle failed: {}", msg);
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
/// Returns array of usage data for all active provider/profile combinations,
/// plus flat fields for the primary account (backwards compatibility).
/// Returns 204 No Content if no credentials are available.
async fn api_usage(State(state): State<Arc<WebState>>) -> Result<impl IntoResponse, StatusCode> {
    // Collect active provider/profile combinations from running coworkers
    let active_profiles: Vec<(crate::auth::AuthProvider, String)> = state
        .coworkers
        .as_ref()
        .map(|cm| {
            let coworkers = cm.list();
            let mut seen = std::collections::HashSet::new();
            let mut profiles = Vec::new();
            for cw in coworkers {
                let key = (cw.provider, cw.profile.clone());
                if seen.insert(key.clone()) {
                    profiles.push(key);
                }
            }
            profiles
        })
        .unwrap_or_default();

    // If no active coworkers, fall back to configured project-lead provider/profile.
    let profiles_to_fetch = if active_profiles.is_empty() {
        let provider = crate::config::get_execution_provider_for_role(
            &state.config.repo,
            crate::config::ExecutionRole::Lead,
        );
        let profile =
            crate::auth::active_profile_for_project_with_provider(&state.config.repo, provider);
        vec![(provider, profile)]
    } else {
        active_profiles
    };

    // Build a cache key from the sorted profile set so the cache invalidates
    // naturally when coworkers spawn or shut down (changing the active profiles).
    let cache_key = {
        let mut sorted = profiles_to_fetch.clone();
        sorted.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()).then_with(|| a.1.cmp(&b.1)));
        sorted
            .iter()
            .map(|(p, n)| format!("{}:{}", p.as_str(), n))
            .collect::<Vec<_>>()
            .join(",")
    };

    let data = tokio::task::spawn_blocking(move || {
        MULTI_USAGE_CACHE
            .get(USAGE_CACHE_TTL, &cache_key)
            .or_else(|| {
                let data = crate::usage::fetch_multi_usage(&profiles_to_fetch);
                if data.is_empty() {
                    None
                } else {
                    MULTI_USAGE_CACHE.set(cache_key, data.clone());
                    Some(data)
                }
            })
    })
    .await
    .map_err(|e| {
        error!("Failed to spawn blocking task for usage: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    match data {
        Some(usage_list) if !usage_list.is_empty() => {
            // Primary account (first in list) for backwards compatibility
            let primary = &usage_list[0];

            // Convert to JSON array
            let usage_array: Vec<serde_json::Value> = usage_list
                .iter()
                .map(|u| {
                    serde_json::json!({
                        "provider": u.provider.as_str(),
                        "profile": u.profile_name,
                        "session_util": u.session_util,
                        "session_resets": u.session_resets.as_ref().map(|d| d.to_rfc3339()),
                        "week_util": u.week_util,
                        "week_resets": u.week_resets.as_ref().map(|d| d.to_rfc3339()),
                        "account_email": u.account_email,
                    })
                })
                .collect();

            Ok(axum::Json(serde_json::json!({
                "usage": usage_array,
                // Backwards compatibility: flat fields for primary account
                "session_util": primary.session_util,
                "session_resets": primary.session_resets.as_ref().map(|d| d.to_rfc3339()),
                "week_util": primary.week_util,
                "week_resets": primary.week_resets.as_ref().map(|d| d.to_rfc3339()),
                "account_email": primary.account_email,
            })))
        }
        _ => Err(StatusCode::NO_CONTENT),
    }
}

/// Upload a file (image or document) from the web UI.
///
/// Accepts multipart/form-data with a file field. Saves the file to
/// Returns pending coworker questions for initial hydration on page load.
async fn api_pending_questions(
    State(state): State<Arc<WebState>>,
) -> Result<impl IntoResponse, StatusCode> {
    let repo = state.config.repo.clone();
    let questions = tokio::task::spawn_blocking(move || {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;

        let socket = crate::paths::daemon_socket_for_repo(&repo);
        let mut stream = UnixStream::connect(&socket).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "coworker.questions",
            "id": 1
        });
        writeln!(stream, "{}", request).ok()?;
        stream.flush().ok()?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;

        let resp: serde_json::Value = serde_json::from_str(&line).ok()?;
        resp.get("result").and_then(|r| r.get("questions")).cloned()
    })
    .await
    .unwrap_or(None)
    .unwrap_or_else(|| serde_json::json!([]));

    Ok(axum::Json(serde_json::json!({ "questions": questions })))
}

/// `~/.midtown/projects/<repo>/uploads/<timestamp>-<filename>` and returns
/// the absolute path for the lead to read.
///
/// Files are stored with a timestamp prefix to avoid collisions and allow
/// easy chronological sorting.
async fn api_upload(
    State(state): State<Arc<WebState>>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, (StatusCode, axum::Json<serde_json::Value>)> {
    // Create uploads directory if it doesn't exist
    let uploads_dir = crate::paths::projects_dir_for_repo(&state.config.repo).join("uploads");
    if let Err(e) = tokio::fs::create_dir_all(&uploads_dir).await {
        error!("Failed to create uploads directory: {}", e);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({ "error": "Failed to create uploads directory" })),
        ));
    }

    // Process the multipart upload
    loop {
        let field: axum::extract::multipart::Field<'_> = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                error!("Failed to read multipart field: {}", e);
                return Err((
                    StatusCode::BAD_REQUEST,
                    axum::Json(serde_json::json!({ "error": "Invalid multipart data" })),
                ));
            }
        };

        let field_name = field.name().unwrap_or("").to_string();
        if field_name != "file" {
            continue;
        }

        let filename = field
            .file_name()
            .map(|s: &str| s.to_string())
            .unwrap_or_else(|| "upload".to_string());

        // Validate filename (prevent directory traversal)
        if filename.contains("..") || filename.contains('/') || filename.contains('\\') {
            return Err((
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({ "error": "Invalid filename" })),
            ));
        }

        let data = match field.bytes().await {
            Ok(bytes) => bytes,
            Err(e) => {
                error!("Failed to read file data: {}", e);
                return Err((
                    StatusCode::BAD_REQUEST,
                    axum::Json(serde_json::json!({ "error": "Failed to read file data" })),
                ));
            }
        };

        // Enforce max file size (10MB)
        const MAX_FILE_SIZE: usize = 10 * 1024 * 1024;
        if data.len() > MAX_FILE_SIZE {
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                axum::Json(serde_json::json!({ "error": "File too large (max 10MB)" })),
            ));
        }

        // Generate timestamped filename
        let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        let safe_filename = format!("{}-{}", timestamp, filename);
        let file_path = uploads_dir.join(&safe_filename);

        // Write file to disk
        if let Err(e) = tokio::fs::write(&file_path, &data).await {
            error!("Failed to write uploaded file: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({ "error": "Failed to save file" })),
            ));
        }

        info!(
            "Uploaded file: {} ({} bytes) -> {}",
            filename,
            data.len(),
            file_path.display()
        );

        // Return the absolute path so the lead can read it
        return Ok(axum::Json(serde_json::json!({
            "path": file_path.to_string_lossy().to_string(),
            "filename": safe_filename,
        })));
    }

    Err((
        StatusCode::BAD_REQUEST,
        axum::Json(serde_json::json!({ "error": "No file provided" })),
    ))
}

/// Serve a previously uploaded file by filename.
///
/// Files are served from `~/.midtown/projects/<repo>/uploads/<filename>`.
/// The filename must not contain path separators or `..` to prevent traversal.
async fn api_get_upload(
    State(state): State<Arc<WebState>>,
    Path(filename): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    if filename.contains("..") || filename.contains('/') || filename.contains('\\') {
        return Err(StatusCode::BAD_REQUEST);
    }

    let file_path = crate::paths::projects_dir_for_repo(&state.config.repo)
        .join("uploads")
        .join(&filename);

    let data = tokio::fs::read(&file_path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

    let content_type = match file_path.extension().and_then(|e| e.to_str()) {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    };

    Ok(([(axum::http::header::CONTENT_TYPE, content_type)], data))
}

/// WebSocket upgrade handler
async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<WebState>>) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_websocket(socket, state))
}

/// Handle an individual WebSocket connection
async fn handle_websocket(socket: WebSocket, state: Arc<WebState>) {
    let (mut sender, mut receiver) = socket.split();

    debug!("WebSocket connection opened");

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
                if let Err(e) = handle_client_message(&text, &state_clone).await {
                    warn!("Error handling client message: {}", e);
                    // Send error back to client using try_send to avoid blocking.
                    // If the channel is full (client is slow), log and drop the error.
                    if let Err(send_err) = error_tx.try_send(e) {
                        warn!("Failed to send error to WebSocket client: {:?}", send_err);
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

    send_task.abort();
    debug!("WebSocket connection closed");
}

/// Handle a message from a WebSocket client
async fn handle_client_message(text: &str, state: &Arc<WebState>) -> Result<(), String> {
    let msg: ClientMessage =
        serde_json::from_str(text).map_err(|e| format!("Invalid message format: {}", e))?;

    match msg {
        ClientMessage::SendMessage {
            content,
            channel,
            thread_parent_id,
        } => {
            // Forward to the daemon for processing (handles channel write,
            // WebSocket broadcast, and side-effects like nudging the Lead)
            state
                .channel_post_tx
                .send(MobileChannelPost {
                    content: content.clone(),
                    channel: channel.clone(),
                    thread_parent_id: thread_parent_id.clone(),
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

            // Nudge delivery via tmux has been removed. Lead nudges now
            // flow through the headed intercom queue, not the web UI.
            return Err("Lead nudge via web UI is no longer supported (tmux removed)".to_string());
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

            // send_key via tmux has been removed. All coworkers are
            // headless and controlled through SessionManager.
            return Err(format!(
                "Send key to {} via web UI is no longer supported (tmux removed)",
                target
            ));
        }
        ClientMessage::AnswerQuestion {
            coworker_name,
            answer,
        } => {
            // Validate inputs
            if coworker_name.is_empty()
                || !coworker_name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            {
                return Err("Invalid coworker name".to_string());
            }
            if answer.is_empty() {
                return Err("Empty answer".to_string());
            }

            // Forward to daemon via coworker.nudge RPC (which clears the pending question and delivers the answer).
            // Uses spawn_blocking to avoid stalling the async executor on synchronous socket I/O.
            let repo = state.config.repo.clone();
            let cw_name = coworker_name.clone();
            let ans = answer.clone();
            tokio::task::spawn_blocking(move || {
                use std::io::{BufRead, BufReader, Write};
                use std::os::unix::net::UnixStream;
                let socket = crate::paths::daemon_socket_for_repo(&repo);
                let mut stream = UnixStream::connect(&socket)
                    .map_err(|e| format!("Failed to connect to daemon: {}", e))?;
                stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
                stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                let request = serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "coworker.nudge",
                    "params": {
                        "from": "lead",
                        "name": cw_name,
                        "message": ans,
                    },
                    "id": 1
                });
                writeln!(stream, "{}", request)
                    .map_err(|e| format!("Failed to send nudge: {}", e))?;
                stream.flush().ok();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).ok();
                Ok::<(), String>(())
            })
            .await
            .map_err(|e| format!("spawn_blocking panic in answer_question: {}", e))?
            .map_err(|e: String| e)?;
            info!("Answered question from {}: {}", coworker_name, answer);
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
        status: Some(status.to_string()),
        current_task: current_task.map(|s| s.to_string()),
        model: Some(model.to_string()),
        session_id: None,
        phase: None,
        progress: None,
        time_estimate: None,
        health: None,
    })
}

/// Build a `WebUpdate` for a coworker progress/phase change.
///
/// Sent when a coworker reports state via `midtown state <phase> --progress <pct>`.
/// This allows the web UI to update progress bars and ETA in real time without
/// waiting for the next 30s status poll.
pub fn coworker_progress_update(
    name: &str,
    phase: Option<String>,
    progress: Option<u8>,
    time_estimate: Option<String>,
    health: Option<String>,
) -> WebUpdate {
    WebUpdate::CoworkerStatus(CoworkerStatusData {
        name: name.to_string(),
        status: None,
        current_task: None,
        model: None,
        session_id: None,
        phase,
        progress,
        time_estimate,
        health,
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

/// Build a `WebUpdate` for a channel message.
pub fn channel_message_update(message: &Message) -> WebUpdate {
    WebUpdate::ChannelMessage(ChannelMessageData {
        id: message.id.clone(),
        from: message.from.clone(),
        content: message.content.clone(),
        timestamp: message.timestamp.to_rfc3339(),
        msg_type: format!("{:?}", message.message_type).to_lowercase(),
        channel: message.channel_name().to_string(),
        thread_parent_id: message.thread_parent_id.clone(),
        reply_count: None,
        last_reply: None,
    })
}

// ── Thread fork/unfork/ownership API ──────────────────────────────────────────

#[derive(Deserialize)]
struct ThreadForkRequest {
    thread_parent_id: String,
    channel_name: String,
}

#[derive(Deserialize)]
struct ThreadUnforkRequest {
    thread_parent_id: String,
}

#[derive(Deserialize)]
struct ThreadOwnershipQuery {
    thread_parent_id: String,
    channel_name: String,
}

async fn api_thread_fork(
    State(state): State<Arc<WebState>>,
    Json(body): Json<ThreadForkRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let rpc_tx = state.web_rpc_tx.as_ref().ok_or_else(|| {
        warn!("api_thread_fork: no web_rpc_tx available");
        StatusCode::SERVICE_UNAVAILABLE
    })?;

    let (tx, rx) = tokio::sync::oneshot::channel();
    rpc_tx
        .send(WebRpcRequest::ForkThread {
            thread_parent_id: body.thread_parent_id,
            channel_name: body.channel_name,
            response_tx: tx,
        })
        .await
        .map_err(|_| {
            error!("api_thread_fork: failed to send to daemon");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    match rx.await {
        Ok(Ok(value)) => Ok(Json(value)),
        Ok(Err(e)) => {
            warn!("api_thread_fork: daemon error: {}", e);
            Ok(Json(serde_json::json!({ "error": e })))
        }
        Err(_) => {
            error!("api_thread_fork: response channel dropped");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn api_thread_unfork(
    State(state): State<Arc<WebState>>,
    Json(body): Json<ThreadUnforkRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let rpc_tx = state.web_rpc_tx.as_ref().ok_or_else(|| {
        warn!("api_thread_unfork: no web_rpc_tx available");
        StatusCode::SERVICE_UNAVAILABLE
    })?;

    let (tx, rx) = tokio::sync::oneshot::channel();
    rpc_tx
        .send(WebRpcRequest::UnforkThread {
            thread_parent_id: body.thread_parent_id,
            response_tx: tx,
        })
        .await
        .map_err(|_| {
            error!("api_thread_unfork: failed to send to daemon");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    match rx.await {
        Ok(Ok(value)) => Ok(Json(value)),
        Ok(Err(e)) => {
            warn!("api_thread_unfork: daemon error: {}", e);
            Ok(Json(serde_json::json!({ "error": e })))
        }
        Err(_) => {
            error!("api_thread_unfork: response channel dropped");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn api_thread_ownership(
    State(state): State<Arc<WebState>>,
    Query(params): Query<ThreadOwnershipQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let rpc_tx = state.web_rpc_tx.as_ref().ok_or_else(|| {
        warn!("api_thread_ownership: no web_rpc_tx available");
        StatusCode::SERVICE_UNAVAILABLE
    })?;

    let (tx, rx) = tokio::sync::oneshot::channel();
    rpc_tx
        .send(WebRpcRequest::ThreadOwnership {
            thread_parent_id: params.thread_parent_id,
            channel_name: params.channel_name,
            response_tx: tx,
        })
        .await
        .map_err(|_| {
            error!("api_thread_ownership: failed to send to daemon");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    match rx.await {
        Ok(Ok(info)) => Ok(Json(serde_json::json!(info))),
        Ok(Err(e)) => {
            warn!("api_thread_ownership: daemon error: {}", e);
            Ok(Json(serde_json::json!({ "error": e })))
        }
        Err(_) => {
            error!("api_thread_ownership: response channel dropped");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Broadcast a new channel message to all WebSocket clients
pub fn broadcast_channel_message(tx: &broadcast::Sender<WebUpdate>, message: &Message) {
    let _ = tx.send(channel_message_update(message));
}

/// Build a `WebUpdate` for a channel list change (create, archive, unarchive, rename).
pub fn channel_list_changed(action: &str, channel: &str) -> WebUpdate {
    WebUpdate::ChannelListChanged(ChannelListChangedData {
        action: action.to_string(),
        channel: channel.to_string(),
    })
}

#[path = "web_tests.rs"]
#[cfg(test)]
mod tests;
