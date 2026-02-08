//! GitHub webhook integration
//!
//! Receives GitHub webhook events and translates them to channel messages.
//!
//! ## Supported Events
//!
//! - `pull_request` - PR opened, closed, merged, etc.
//! - `pull_request_review` - Review submitted (approved, changes requested, commented)
//! - `issue_comment` / `pull_request_review_comment` - Comments added
//! - `status` / `check_run` - CI status changes
//!
//! ## Security
//!
//! Webhook payloads are verified using HMAC-SHA256 with the configured secret.
//! The signature is sent in the `X-Hub-Signature-256` header.

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tower_http::cors::{Any, CorsLayer};
use tracing::{debug, error, info, warn};

use crate::coworker::CoworkerManager;
use crate::message::{Message, MessageType};
use crate::web::{self, MobileChannelPost, WebConfig, WebState, WebUpdate};

type HmacSha256 = Hmac<Sha256>;

/// A webhook event with an optional structured PR activity payload.
///
/// This allows the daemon to act on PR activity (e.g., nudging PR owners)
/// without parsing message content strings or making extra GitHub API calls.
#[derive(Debug, Clone)]
pub struct WebhookEvent {
    /// The channel message to post
    pub message: Message,
    /// Structured PR activity data (if this event relates to a PR)
    pub pr_activity: Option<PrActivity>,
    /// PR number that needs a reviewer spawned (set on "opened" / "ready_for_review")
    pub needs_review: Option<u64>,
    /// PR number that was just merged (set on "closed" with merged=true)
    pub merged_pr: Option<u64>,
    /// PR merged info (title + number) for task auto-completion
    pub pr_merged_info: Option<PrMergedInfo>,
    /// If set, a CI check failed on the default branch — nudge the lead with this message
    pub ci_failed_on_default_branch: Option<String>,
    /// PR number that received a Claude review comment (for caching review status).
    /// Set when an `issue_comment` webhook contains a review signature.
    pub reviewed_pr: Option<u64>,
    /// A formal review state change (approved / changes_requested) — triggers immediate
    /// nudge of the PR owner instead of waiting for the next polling cycle.
    pub review_state_change: Option<PrReviewStateChange>,
    /// A CI check failure on a PR branch — triggers immediate nudge of the PR owner.
    pub pr_ci_failure: Option<PrCiFailure>,
    /// A completed CI check with its duration — used for tracking typical check times.
    pub check_duration: Option<CheckDuration>,
    /// A successful CI check that can be batched with others for the same target.
    /// When set, the daemon buffers this notification instead of posting `message`
    /// immediately — `message` is ignored in favor of a later batched message.
    pub ci_check_passed: Option<CiCheckPassed>,
    /// Info about a newly opened PR — used to store the author's session for handoff.
    pub pr_opened: Option<PrOpenedInfo>,
}

impl WebhookEvent {
    /// Create a new WebhookEvent with the given message, defaulting all optional fields to None.
    ///
    /// Use struct update syntax to override specific fields:
    /// ```ignore
    /// WebhookEvent {
    ///     needs_review: Some(42),
    ///     ..WebhookEvent::new(message)
    /// }
    /// ```
    pub fn new(message: Message) -> Self {
        Self {
            message,
            pr_activity: None,
            needs_review: None,
            merged_pr: None,
            pr_merged_info: None,
            ci_failed_on_default_branch: None,
            reviewed_pr: None,
            review_state_change: None,
            pr_ci_failure: None,
            check_duration: None,
            ci_check_passed: None,
            pr_opened: None,
        }
    }

    /// Create a WebhookEvent from GitHub with the given content string.
    ///
    /// This is a convenience constructor that wraps the content in a standard
    /// GitHub message (author="github", type=Text).
    ///
    /// Use struct update syntax to override specific fields:
    /// ```ignore
    /// WebhookEvent {
    ///     needs_review: Some(42),
    ///     ..WebhookEvent::github(content)
    /// }
    /// ```
    pub fn github(content: impl Into<String>) -> Self {
        Self::new(Message::new("github", content, MessageType::Text))
    }
}

/// Structured data about a completed CI check's duration.
///
/// Used to track historical check durations so the daemon can detect stale checks
/// (running > 4x typical duration).
#[derive(Debug, Clone)]
pub struct CheckDuration {
    /// Name of the check (e.g., "Test", "E2E - idle_break_e2e")
    pub check_name: String,
    /// Duration in seconds
    pub duration_secs: u64,
}

/// Structured data about a successful CI check that can be batched.
///
/// When multiple CI checks pass within a short window on the same target
/// (PR or branch), the daemon batches them into a single channel message.
#[derive(Debug, Clone)]
pub struct CiCheckPassed {
    /// Name of the check that passed
    pub check_name: String,
    /// Target reference: "main", "PR #42", etc.
    pub target: String,
    /// Mention prefix for the coworker (e.g., "@columbus ") or empty string
    pub mention_prefix: String,
}

/// Structured data about a formal PR review state change (approved or changes requested).
///
/// Populated by the `pull_request_review` webhook handler so the daemon can
/// immediately nudge the PR owner rather than waiting for the next poll cycle.
#[derive(Debug, Clone)]
pub struct PrReviewStateChange {
    /// PR number
    pub pr_number: u64,
    /// The coworker who owns the PR (from branch prefix or body frontmatter)
    pub owner_coworker: Option<String>,
    /// The reviewer who submitted the review
    pub reviewer: String,
    /// Whether the review was approved or requested changes
    pub state: ReviewState,
}

/// The state of a formal PR review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewState {
    Approved,
    ChangesRequested,
}

/// Structured data about a CI check failure on a PR branch.
///
/// Populated by the `check_run` webhook handler so the daemon can
/// immediately nudge the PR owner rather than waiting for the next poll cycle.
#[derive(Debug, Clone)]
pub struct PrCiFailure {
    /// PR number
    pub pr_number: u64,
    /// The coworker who owns the PR (from branch prefix or body frontmatter)
    pub owner_coworker: Option<String>,
    /// Name of the failed check
    pub check_name: String,
}

/// Structured data about a newly opened PR.
///
/// Populated by the `pull_request` webhook handler when a PR is opened,
/// allowing the daemon to store the author's session ID for potential handoff.
#[derive(Debug, Clone)]
pub struct PrOpenedInfo {
    /// PR number
    pub pr_number: u64,
    /// The branch name (e.g., "lexington/feature-auth")
    pub branch: String,
    /// The coworker who opened the PR (from branch prefix or body frontmatter)
    pub author_coworker: Option<String>,
    /// The PR title (no longer used for task completion - see PrMergedInfo)
    pub title: String,
}

/// Populated by the `pull_request` webhook handler when a PR is merged,
/// allowing the daemon to auto-complete tasks when implementation is done.
#[derive(Debug, Clone)]
pub struct PrMergedInfo {
    /// PR number
    pub pr_number: u64,
    /// The PR title (for extracting task ID from "[Midtown #XX]" format)
    pub title: String,
}

/// Identifies a GitHub comment for the reactions API.
#[derive(Debug, Clone)]
pub enum CommentNode {
    /// Issue comment: `/repos/{owner}/{repo}/issues/comments/{id}/reactions`
    IssueComment(u64),
    /// Pull request review comment: `/repos/{owner}/{repo}/pulls/comments/{id}/reactions`
    ReviewComment(u64),
    /// Pull request review: `/repos/{owner}/{repo}/pulls/{pull}/reviews/{id}/reactions`
    Review { pull: u64, review_id: u64 },
}

/// Structured data about PR-related webhook activity.
#[derive(Debug, Clone)]
pub struct PrActivity {
    /// PR number
    pub pr_number: u64,
    /// The coworker who owns the PR (from branch prefix or body frontmatter)
    pub owner_coworker: Option<String>,
    /// The PR branch name (used to detect lead/* branches)
    pub branch: Option<String>,
    /// The actor who triggered the event (coworker name or GitHub username)
    pub actor: String,
    /// The comment/review node for adding reactions
    pub comment_node: Option<CommentNode>,
    /// The repository full name (owner/repo) for API calls
    pub repo_full_name: Option<String>,
}

/// Configuration for the webhook server
#[derive(Debug, Clone)]
pub struct WebhookConfig {
    /// Port to listen on
    pub port: u16,
    /// Webhook secret for signature verification (optional but recommended)
    pub secret: Option<String>,
    /// Repository name for channel routing
    pub repo: String,
}

impl Default for WebhookConfig {
    fn default() -> Self {
        Self {
            port: 8080,
            secret: None,
            repo: "default".to_string(),
        }
    }
}

/// Shared state for the webhook server
struct WebhookState {
    config: WebhookConfig,
    event_tx: mpsc::Sender<WebhookEvent>,
}

/// GitHub webhook event header
const GITHUB_EVENT_HEADER: &str = "X-GitHub-Event";
/// GitHub signature header (SHA256)
const GITHUB_SIGNATURE_HEADER: &str = "X-Hub-Signature-256";

/// Start the webhook HTTP server
///
/// Returns a channel receiver for translated messages, a broadcast sender
/// for pushing real-time updates to WebSocket clients, a receiver for
/// mobile channel posts that need to be processed by the daemon, and
/// the shared push notification manager.
pub async fn start_webhook_server(
    config: WebhookConfig,
    coworker_manager: Option<CoworkerManager>,
    all_repo_paths: Vec<std::path::PathBuf>,
    default_branch: String,
) -> crate::Result<(
    mpsc::Receiver<WebhookEvent>,
    broadcast::Sender<WebUpdate>,
    mpsc::Receiver<MobileChannelPost>,
    Option<Arc<crate::push::PushManager>>,
)> {
    let (tx, rx) = mpsc::channel(100);
    let (web_updates_tx, _) = broadcast::channel(100);
    let (mobile_tx, mobile_rx) = mpsc::channel(100);

    let webhook_state = Arc::new(WebhookState {
        config: config.clone(),
        event_tx: tx,
    });

    // Create web state for mobile app
    let web_config = WebConfig {
        repo: config.repo.clone(),
    };

    let push_manager: Option<Arc<crate::push::PushManager>> = match crate::push::PushManager::new()
    {
        Ok(pm) => {
            tracing::info!("Web Push notification manager initialized");
            Some(Arc::new(pm))
        }
        Err(e) => {
            tracing::warn!("Failed to initialize push manager: {}", e);
            None
        }
    };

    let tmux_session = format!("{}{}", crate::tmux::SESSION_PREFIX, config.repo);
    let web_state = Arc::new(WebState {
        config: web_config,
        updates_tx: web_updates_tx.clone(),
        coworkers: coworker_manager,
        channel_post_tx: mobile_tx,
        push_manager: push_manager.clone(),
        all_repo_paths,
        default_branch,
        repo_name_cache: std::sync::RwLock::new(std::collections::HashMap::new()),
        viewer_tracker: std::sync::Mutex::new(crate::web::ViewerTracker::new(tmux_session)),
    });

    // CORS layer for development (allows requests from Vite dev server)
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // Build the combined router
    let app = Router::new()
        // GitHub webhook endpoint
        .route("/webhook", post(handle_webhook))
        .route("/health", axum::routing::get(health_check))
        .with_state(webhook_state)
        // Merge the web app router (API + static files)
        .merge(web::create_web_router(web_state))
        .layer(cors);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    info!("Starting webhook server on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            error!("Webhook server error: {}", e);
        }
    });

    Ok((rx, web_updates_tx, mobile_rx, push_manager))
}

/// Health check endpoint
async fn health_check() -> &'static str {
    "ok"
}

/// Handle incoming webhook requests
async fn handle_webhook(
    State(state): State<Arc<WebhookState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, StatusCode> {
    // Get event type
    let event_type = headers
        .get(GITHUB_EVENT_HEADER)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            warn!("Missing X-GitHub-Event header");
            StatusCode::BAD_REQUEST
        })?;

    debug!("Received GitHub event: {}", event_type);

    // Verify signature if secret is configured
    if let Some(ref secret) = state.config.secret {
        let signature = headers
            .get(GITHUB_SIGNATURE_HEADER)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                warn!("Missing signature header");
                StatusCode::UNAUTHORIZED
            })?;

        if !verify_signature(secret, &body, signature) {
            warn!("Invalid webhook signature");
            return Err(StatusCode::UNAUTHORIZED);
        }
    }

    // Parse and handle the event
    let event = match event_type {
        "pull_request" => handle_pull_request(&body),
        "pull_request_review" => handle_pull_request_review(&body),
        "issue_comment" => handle_issue_comment(&body),
        "pull_request_review_comment" => handle_review_comment(&body),
        "status" => handle_status(&body),
        "check_run" => handle_check_run(&body),
        "ping" => {
            info!("Received ping event - webhook is configured correctly");
            Ok(None)
        }
        _ => {
            debug!("Ignoring unhandled event type: {}", event_type);
            Ok(None)
        }
    };

    match event {
        Ok(Some(webhook_event)) => {
            if let Err(e) = state.event_tx.send(webhook_event).await {
                error!("Failed to send webhook event: {}", e);
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
            Ok(StatusCode::OK)
        }
        Ok(None) => Ok(StatusCode::OK),
        Err(e) => {
            warn!("Failed to parse webhook payload: {}", e);
            Err(StatusCode::BAD_REQUEST)
        }
    }
}

/// Verify GitHub webhook signature
fn verify_signature(secret: &str, payload: &[u8], signature: &str) -> bool {
    // Signature format: "sha256=<hex>"
    let expected = match signature.strip_prefix("sha256=") {
        Some(hex) => hex,
        None => return false,
    };

    let expected_bytes = match hex::decode(expected) {
        Ok(bytes) => bytes,
        Err(_) => return false,
    };

    let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(mac) => mac,
        Err(_) => return false,
    };

    mac.update(payload);

    mac.verify_slice(&expected_bytes).is_ok()
}

// ============================================================================
// GitHub Event Payloads (simplified)
// ============================================================================

#[derive(Debug, Deserialize)]
struct PullRequestEvent {
    action: String,
    number: u64,
    pull_request: PullRequest,
    #[allow(dead_code)]
    repository: Repository,
}

#[derive(Debug, Deserialize)]
struct PullRequest {
    title: String,
    #[allow(dead_code)]
    user: User,
    merged: Option<bool>,
    head: Option<PullRequestHead>,
    body: Option<String>,
    #[serde(default)]
    draft: bool,
}

#[derive(Debug, Deserialize)]
struct PullRequestHead {
    #[serde(rename = "ref")]
    branch: String,
}

#[derive(Debug, Deserialize)]
struct PullRequestReviewEvent {
    action: String,
    review: Review,
    pull_request: PullRequestRef,
    repository: Repository,
}

#[derive(Debug, Deserialize)]
struct Review {
    id: u64,
    state: String,
    user: User,
}

#[derive(Debug, Deserialize)]
struct PullRequestRef {
    number: u64,
    head: Option<PullRequestHead>,
    body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IssueCommentEvent {
    action: String,
    issue: Issue,
    comment: Comment,
    repository: Repository,
}

#[derive(Debug, Deserialize)]
struct Issue {
    number: u64,
    pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct Comment {
    id: u64,
    user: User,
    body: String,
}

#[derive(Debug, Deserialize)]
struct ReviewCommentEvent {
    action: String,
    pull_request: PullRequestRef,
    comment: Comment,
    repository: Repository,
}

#[derive(Debug, Deserialize)]
struct StatusEvent {
    state: String,
    context: String,
    description: Option<String>,
    #[allow(dead_code)]
    sha: String,
    branches: Option<Vec<StatusBranch>>,
    #[allow(dead_code)]
    repository: Repository,
}

#[derive(Debug, Deserialize)]
struct StatusBranch {
    name: String,
}

#[derive(Debug, Deserialize)]
struct CheckRunEvent {
    #[allow(dead_code)]
    action: String,
    check_run: CheckRun,
    #[allow(dead_code)]
    repository: Repository,
}

#[derive(Debug, Deserialize)]
struct CheckRun {
    name: String,
    status: String,
    conclusion: Option<String>,
    check_suite: Option<CheckSuite>,
    /// When the check started (ISO 8601 timestamp)
    started_at: Option<String>,
    /// When the check completed (ISO 8601 timestamp)
    completed_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CheckSuite {
    #[allow(dead_code)]
    head_sha: String,
    head_branch: Option<String>,
    pull_requests: Vec<CheckSuitePR>,
}

#[derive(Debug, Deserialize)]
struct CheckSuitePR {
    number: u64,
}

#[derive(Debug, Deserialize)]
struct User {
    login: String,
}

#[derive(Debug, Deserialize)]
struct Repository {
    #[allow(dead_code)]
    full_name: String,
    default_branch: Option<String>,
}

// ============================================================================
// Coworker Attribution
// ============================================================================

/// Known coworker names (avenue names from Manhattan)
const COWORKER_NAMES: &[&str] = &[
    "lexington",
    "park",
    "madison",
    "broadway",
    "amsterdam",
    "columbus",
    "central",
    "riverside",
];

/// Extract coworker name from branch prefix (e.g., "lexington/fix-auth" -> "lexington").
///
/// **Note:** This only supports legacy `<coworker>/<description>` branches. Task-based
/// branches (`task-*`, `review-pr-*`) are handled via frontmatter (`<!-- midtown: name -->`)
/// in the PR body. Since webhooks don't have access to the worktree registry, they rely on
/// coworkers including frontmatter in their PRs for correct attribution.
fn coworker_from_branch(branch: &str) -> Option<&'static str> {
    let prefix = branch.split('/').next()?;
    COWORKER_NAMES
        .iter()
        .find(|&&name| name.eq_ignore_ascii_case(prefix))
        .copied()
}

/// Extract coworker name from frontmatter in body (e.g., "<!-- midtown: lexington -->")
fn coworker_from_frontmatter(body: &str) -> Option<&'static str> {
    // Look for <!-- midtown: name --> pattern
    let start = body.find("<!-- midtown:")?;
    let after_start = &body[start + 13..];
    let end = after_start.find("-->")?;
    let name = after_start[..end].trim();

    COWORKER_NAMES
        .iter()
        .find(|&&n| n.eq_ignore_ascii_case(name))
        .copied()
}

/// Determine the coworker associated with a PR-related event.
/// Priority: frontmatter > branch prefix
/// Returns None if no coworker can be determined.
fn determine_pr_coworker(branch: Option<&str>, body: Option<&str>) -> Option<&'static str> {
    // Check frontmatter first (explicit attribution)
    if let Some(body) = body
        && let Some(name) = coworker_from_frontmatter(body)
    {
        return Some(name);
    }

    // Check branch prefix
    if let Some(branch) = branch
        && let Some(name) = coworker_from_branch(branch)
    {
        return Some(name);
    }

    None
}

/// Format @mention prefix for a coworker, or empty string if none.
fn mention_prefix(coworker: Option<&str>) -> String {
    match coworker {
        Some(name) => format!("@{} ", name),
        None => String::new(),
    }
}

/// Determine the commenter identity from a comment body.
///
/// Priority: comment frontmatter > PR coworker (from branch/body) > GitHub username.
/// The `pr_coworker` fallback handles the common case where a coworker posts a comment
/// without the `<!-- midtown: name -->` signature — we infer their identity from the PR
/// owner. This only applies when the commenter is the repo owner (the shared GitHub
/// account used by all coworkers), since external users should keep their username.
fn commenter_identity(
    comment_body: &str,
    github_username: &str,
    pr_coworker: Option<&str>,
    repo_owner: Option<&str>,
) -> String {
    if let Some(coworker) = coworker_from_frontmatter(comment_body) {
        return coworker.to_string();
    }
    // Only fall back to PR coworker when the GitHub username matches the repo owner
    // (the shared account). External users (bots, other humans) keep their username.
    if let Some(coworker) = pr_coworker {
        let is_repo_owner =
            repo_owner.is_some_and(|owner| owner.eq_ignore_ascii_case(github_username));
        if is_repo_owner {
            return coworker.to_string();
        }
    }
    github_username.to_string()
}

/// Extract the owner (username) from a repository full_name like "owner/repo".
fn repo_owner(full_name: &str) -> Option<&str> {
    full_name.split('/').next()
}

/// Strip the midtown frontmatter line from a comment body.
///
/// Returns the body with the `<!-- midtown: name -->` line removed.
fn strip_frontmatter(body: &str) -> String {
    body.lines()
        .filter(|line| !line.trim().starts_with("<!-- midtown:"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

// ============================================================================
// Event Handlers
// ============================================================================

fn handle_pull_request(body: &[u8]) -> Result<Option<WebhookEvent>, serde_json::Error> {
    let event: PullRequestEvent = serde_json::from_slice(body)?;

    // Determine coworker from branch prefix or PR body frontmatter
    let branch = event.pull_request.head.as_ref().map(|h| h.branch.as_str());
    let pr_body = event.pull_request.body.as_deref();
    let coworker = determine_pr_coworker(branch, pr_body);
    let mention = mention_prefix(coworker);

    let action_text = match event.action.as_str() {
        "opened" => format!("opened PR #{}: {}", event.number, event.pull_request.title),
        "closed" => {
            if event.pull_request.merged.unwrap_or(false) {
                format!("merged PR #{}: {}", event.number, event.pull_request.title)
            } else {
                format!(
                    "closed PR #{} (not merged): {}",
                    event.number, event.pull_request.title
                )
            }
        }
        "reopened" => format!(
            "reopened PR #{}: {}",
            event.number, event.pull_request.title
        ),
        "synchronize" => format!("pushed to PR #{}", event.number),
        "ready_for_review" => format!(
            "marked PR #{} ready for review: {}",
            event.number, event.pull_request.title
        ),
        _ => return Ok(None),
    };

    // Trigger immediate reviewer spawn for non-draft PRs that are opened or
    // marked ready for review. Draft PRs and other actions don't need review.
    let needs_review = match event.action.as_str() {
        "opened" if !event.pull_request.draft => Some(event.number),
        "ready_for_review" => Some(event.number),
        _ => None,
    };

    // Flag merged PRs so the daemon can nudge the lead to pull main
    let merged_pr = match event.action.as_str() {
        "closed" if event.pull_request.merged.unwrap_or(false) => Some(event.number),
        _ => None,
    };

    // Capture PR title when merged (for task auto-completion)
    let pr_merged_info = match event.action.as_str() {
        "closed" if event.pull_request.merged.unwrap_or(false) => Some(PrMergedInfo {
            pr_number: event.number,
            title: event.pull_request.title.clone(),
        }),
        _ => None,
    };

    // Capture PR author info when a PR is opened (for session handoff)
    let pr_opened = match event.action.as_str() {
        "opened" | "ready_for_review" => branch.map(|b| PrOpenedInfo {
            pr_number: event.number,
            branch: b.to_string(),
            author_coworker: coworker.map(String::from),
            title: event.pull_request.title.clone(),
        }),
        _ => None,
    };

    let content = format!("{}{}", mention, action_text);
    Ok(Some(WebhookEvent {
        needs_review,
        merged_pr,
        pr_merged_info,
        pr_opened,
        ..WebhookEvent::github(content)
    }))
}

fn handle_pull_request_review(body: &[u8]) -> Result<Option<WebhookEvent>, serde_json::Error> {
    let event: PullRequestReviewEvent = serde_json::from_slice(body)?;

    if event.action != "submitted" {
        return Ok(None);
    }

    // Determine coworker from PR branch prefix or body frontmatter
    let branch = event.pull_request.head.as_ref().map(|h| h.branch.as_str());
    let pr_body = event.pull_request.body.as_deref();
    let coworker = determine_pr_coworker(branch, pr_body);
    let mention = mention_prefix(coworker);

    let action_text = match event.review.state.to_lowercase().as_str() {
        "approved" => format!(
            "{} approved PR #{}",
            event.review.user.login, event.pull_request.number
        ),
        "changes_requested" => format!(
            "{} requested changes on PR #{}",
            event.review.user.login, event.pull_request.number
        ),
        "commented" => format!(
            "{} commented on PR #{}",
            event.review.user.login, event.pull_request.number
        ),
        _ => return Ok(None),
    };

    // Produce a structured review state change for approved/changes_requested
    // so the daemon can immediately nudge the PR owner via webhook.
    let review_state_change = match event.review.state.to_lowercase().as_str() {
        "approved" => Some(PrReviewStateChange {
            pr_number: event.pull_request.number,
            owner_coworker: coworker.map(|s| s.to_string()),
            reviewer: event.review.user.login.clone(),
            state: ReviewState::Approved,
        }),
        "changes_requested" => Some(PrReviewStateChange {
            pr_number: event.pull_request.number,
            owner_coworker: coworker.map(|s| s.to_string()),
            reviewer: event.review.user.login.clone(),
            state: ReviewState::ChangesRequested,
        }),
        _ => None,
    };

    let content = format!("{}{}", mention, action_text);
    Ok(Some(WebhookEvent {
        pr_activity: Some(PrActivity {
            pr_number: event.pull_request.number,
            owner_coworker: coworker.map(|s| s.to_string()),
            branch: branch.map(|s| s.to_string()),
            actor: event.review.user.login,
            comment_node: Some(CommentNode::Review {
                pull: event.pull_request.number,
                review_id: event.review.id,
            }),
            repo_full_name: Some(event.repository.full_name),
        }),
        review_state_change,
        ..WebhookEvent::github(content)
    }))
}

fn handle_issue_comment(body: &[u8]) -> Result<Option<WebhookEvent>, serde_json::Error> {
    let event: IssueCommentEvent = serde_json::from_slice(body)?;

    if event.action != "created" {
        return Ok(None);
    }

    // Only handle PR comments, not issue comments
    if event.issue.pull_request.is_none() {
        return Ok(None);
    }

    // Determine commenter: use coworker name from signature if present, else GitHub username.
    // issue_comment webhooks don't include the PR branch, so no branch-based fallback.
    let commenter = commenter_identity(
        &event.comment.body,
        &event.comment.user.login,
        None,
        repo_owner(&event.repository.full_name),
    );

    // Strip frontmatter from comment before preview
    let clean_body = strip_frontmatter(&event.comment.body);
    let preview = truncate_comment(&clean_body, 50);

    let content = format!(
        "{} commented on PR #{}: {}",
        commenter, event.issue.number, preview
    );

    // Check if this comment is a Claude code review (for review status caching)
    let reviewed_pr = if is_review_comment(&event.comment.body) {
        Some(event.issue.number)
    } else {
        None
    };

    // For issue_comment, the payload doesn't include the PR branch,
    // so owner_coworker and branch are None. The daemon will look them up asynchronously.
    Ok(Some(WebhookEvent {
        pr_activity: Some(PrActivity {
            pr_number: event.issue.number,
            owner_coworker: None,
            branch: None,
            actor: commenter,
            comment_node: Some(CommentNode::IssueComment(event.comment.id)),
            repo_full_name: Some(event.repository.full_name),
        }),
        reviewed_pr,
        ..WebhookEvent::github(content)
    }))
}

fn handle_review_comment(body: &[u8]) -> Result<Option<WebhookEvent>, serde_json::Error> {
    let event: ReviewCommentEvent = serde_json::from_slice(body)?;

    if event.action != "created" {
        return Ok(None);
    }

    // Determine coworker from PR branch prefix or body frontmatter (for @mention)
    let branch = event.pull_request.head.as_ref().map(|h| h.branch.as_str());
    let pr_body = event.pull_request.body.as_deref();
    let coworker = determine_pr_coworker(branch, pr_body);
    let mention = mention_prefix(coworker);

    // Determine commenter: use coworker name from comment signature if present,
    // fall back to PR coworker (from branch/body) when signature is missing
    // and commenter is the repo owner (shared account)
    let commenter = commenter_identity(
        &event.comment.body,
        &event.comment.user.login,
        coworker,
        repo_owner(&event.repository.full_name),
    );

    // Strip frontmatter from comment before preview
    let clean_body = strip_frontmatter(&event.comment.body);
    let preview = truncate_comment(&clean_body, 50);

    let action_text = format!(
        "{} left review comment on PR #{}: {}",
        commenter, event.pull_request.number, preview
    );

    let content = format!("{}{}", mention, action_text);
    Ok(Some(WebhookEvent {
        pr_activity: Some(PrActivity {
            pr_number: event.pull_request.number,
            owner_coworker: coworker.map(|s| s.to_string()),
            branch: branch.map(|s| s.to_string()),
            actor: commenter,
            comment_node: Some(CommentNode::ReviewComment(event.comment.id)),
            repo_full_name: Some(event.repository.full_name),
        }),
        ..WebhookEvent::github(content)
    }))
}

fn handle_status(body: &[u8]) -> Result<Option<WebhookEvent>, serde_json::Error> {
    let event: StatusEvent = serde_json::from_slice(body)?;

    // Determine coworker from first branch in the branches array
    let branch = event
        .branches
        .as_ref()
        .and_then(|branches| branches.first())
        .map(|b| b.name.as_str());
    let coworker = determine_pr_coworker(branch, None);
    let mention = mention_prefix(coworker);

    let action_text = match event.state.as_str() {
        "success" => format!(
            "CI passed ({}): {}",
            event.context,
            event.description.as_deref().unwrap_or("No description")
        ),
        "failure" => format!(
            "CI failed ({}): {}",
            event.context,
            event.description.as_deref().unwrap_or("No description")
        ),
        "error" => format!(
            "CI error ({}): {}",
            event.context,
            event.description.as_deref().unwrap_or("No description")
        ),
        "pending" => return Ok(None), // Don't spam pending statuses
        _ => return Ok(None),
    };

    let content = format!("{}{}", mention, action_text);
    Ok(Some(WebhookEvent::github(content)))
}

fn handle_check_run(body: &[u8]) -> Result<Option<WebhookEvent>, serde_json::Error> {
    let event: CheckRunEvent = serde_json::from_slice(body)?;

    // Only report completed check runs
    if event.check_run.status != "completed" {
        return Ok(None);
    }

    // Determine coworker from branch prefix (check_suite includes head_branch)
    let branch = event
        .check_run
        .check_suite
        .as_ref()
        .and_then(|cs| cs.head_branch.as_deref());
    let coworker = determine_pr_coworker(branch, None);
    let mention = mention_prefix(coworker);

    let pr_info = event
        .check_run
        .check_suite
        .as_ref()
        .and_then(|cs| cs.pull_requests.first())
        .map(|pr| format!(" on PR #{}", pr.number))
        .unwrap_or_else(|| {
            // No PR - show branch name (e.g., "on main")
            branch.map(|b| format!(" on {}", b)).unwrap_or_default()
        });

    let is_failure = matches!(
        event.check_run.conclusion.as_deref(),
        Some("failure") | Some("timed_out")
    );

    let action_text = match event.check_run.conclusion.as_deref() {
        Some("success") => format!("Check '{}' passed{}", event.check_run.name, pr_info),
        Some("failure") => format!("Check '{}' failed{}", event.check_run.name, pr_info),
        Some("cancelled") => format!("Check '{}' cancelled{}", event.check_run.name, pr_info),
        Some("timed_out") => format!("Check '{}' timed out{}", event.check_run.name, pr_info),
        _ => return Ok(None),
    };

    // Check if this failure is on the default branch (not a PR branch)
    let is_on_default_branch = branch.is_some()
        && event.repository.default_branch.as_deref() == branch
        && event
            .check_run
            .check_suite
            .as_ref()
            .and_then(|cs| cs.pull_requests.first())
            .is_none();

    let ci_failed_on_default_branch = if is_failure && is_on_default_branch {
        let default_branch = event
            .repository
            .default_branch
            .as_deref()
            .or(branch)
            .unwrap_or("main");
        Some(format!(
            "@lead CI check '{}' failed on {} — investigate ASAP",
            event.check_run.name, default_branch,
        ))
    } else {
        None
    };

    // Produce a structured CI failure for PR branches so the daemon can
    // immediately nudge the PR owner via webhook.
    let pr_ci_failure = if is_failure {
        event
            .check_run
            .check_suite
            .as_ref()
            .and_then(|cs| cs.pull_requests.first())
            .map(|pr| PrCiFailure {
                pr_number: pr.number,
                owner_coworker: coworker.map(|s| s.to_string()),
                check_name: event.check_run.name.clone(),
            })
    } else {
        None
    };

    // Calculate check duration for stats tracking (only for successful completions)
    let check_duration = if event.check_run.conclusion.as_deref() == Some("success") {
        compute_check_duration(
            event.check_run.started_at.as_deref(),
            event.check_run.completed_at.as_deref(),
            &event.check_run.name,
        )
    } else {
        None
    };

    // Produce a batchable CI success for the daemon to aggregate.
    // Note: We still construct the message above for consistency, but when ci_check_passed
    // is set, the daemon buffers this check and posts a batched message later instead.
    let ci_check_passed = if event.check_run.conclusion.as_deref() == Some("success") {
        // Extract target from pr_info: "main", "PR #42", etc.
        let target = event
            .check_run
            .check_suite
            .as_ref()
            .and_then(|cs| cs.pull_requests.first())
            .map(|pr| format!("PR #{}", pr.number))
            .or_else(|| branch.map(|b| b.to_string()));

        target.map(|t| CiCheckPassed {
            check_name: event.check_run.name.clone(),
            target: t,
            mention_prefix: mention.clone(),
        })
    } else {
        None
    };

    let content = format!("{}{}", mention, action_text);
    Ok(Some(WebhookEvent {
        ci_failed_on_default_branch,
        pr_ci_failure,
        check_duration,
        ci_check_passed,
        ..WebhookEvent::github(content)
    }))
}

/// Compute the duration of a check run from started_at and completed_at timestamps.
fn compute_check_duration(
    started_at: Option<&str>,
    completed_at: Option<&str>,
    check_name: &str,
) -> Option<CheckDuration> {
    use chrono::DateTime;

    let started: DateTime<chrono::Utc> = started_at?.parse().ok()?;
    let completed: DateTime<chrono::Utc> = completed_at?.parse().ok()?;

    let duration = completed.signed_duration_since(started);
    let duration_secs = duration.num_seconds().max(0) as u64;

    // Sanity check: ignore durations over 24 hours (likely bad data)
    if duration_secs > 86400 {
        return None;
    }

    Some(CheckDuration {
        check_name: check_name.to_string(),
        duration_secs,
    })
}

/// Check if a comment body contains a Claude code review signature.
///
/// This uses the same signatures as `text_contains_review_signature` in
/// `daemon/helpers.rs` to detect review comments from webhook payloads.
fn is_review_comment(body: &str) -> bool {
    body.contains("🤖 Reviewed by")
        || body.contains("## Code Review by")
        || body.contains("## No Issues Found")
        || body.contains("<!-- midtown:")
}

/// Truncate a comment for preview, handling multi-line and unicode safely
fn truncate_comment(comment: &str, max_chars: usize) -> String {
    let first_line = comment.lines().next().unwrap_or(comment);

    // Count characters, not bytes, to handle multi-byte UTF-8 safely
    let char_count = first_line.chars().count();
    if char_count <= max_chars {
        first_line.to_string()
    } else {
        // Find the byte index of the max_chars-th character
        let truncate_at = first_line
            .char_indices()
            .nth(max_chars)
            .map(|(i, _)| i)
            .unwrap_or(first_line.len());
        format!("{}...", &first_line[..truncate_at])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_signature() {
        let secret = "test-secret";
        let payload = b"test payload";

        // Generate valid signature
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(payload);
        let result = mac.finalize();
        let signature = format!("sha256={}", hex::encode(result.into_bytes()));

        assert!(verify_signature(secret, payload, &signature));
        assert!(!verify_signature(secret, payload, "sha256=invalid"));
        assert!(!verify_signature(secret, payload, "invalid-format"));
        assert!(!verify_signature("wrong-secret", payload, &signature));
    }

    #[test]
    fn test_truncate_comment() {
        assert_eq!(truncate_comment("short", 10), "short");
        assert_eq!(
            truncate_comment("this is a longer comment", 10),
            "this is a ..."
        );
        assert_eq!(
            truncate_comment("first line\nsecond line", 50),
            "first line"
        );
        // Test unicode safety - should not panic on multi-byte characters
        assert_eq!(
            truncate_comment("Hello 世界! More text here", 8),
            "Hello 世界..."
        );
        assert_eq!(truncate_comment("emoji 👍 test", 7), "emoji 👍...");
    }

    #[test]
    fn test_handle_pull_request_opened() {
        let payload = r#"{
            "action": "opened",
            "number": 42,
            "pull_request": {
                "title": "Add auth endpoint",
                "user": {"login": "btucker"},
                "merged": false,
                "head": {"ref": "lexington/add-auth"}
            },
            "repository": {"full_name": "org/repo"}
        }"#;

        let event = handle_pull_request(payload.as_bytes()).unwrap().unwrap();
        // Content includes @mention prefix for coworker
        assert_eq!(
            event.message.content,
            "@lexington opened PR #42: Add auth endpoint"
        );
        // Sender is always "github"
        assert_eq!(event.message.from, "github");
        // Non-draft opened PR triggers review spawn
        assert_eq!(event.needs_review, Some(42));
    }

    #[test]
    fn test_handle_pull_request_opened_draft_no_review() {
        let payload = r#"{
            "action": "opened",
            "number": 42,
            "pull_request": {
                "title": "WIP: Add auth endpoint",
                "user": {"login": "btucker"},
                "merged": false,
                "draft": true,
                "head": {"ref": "lexington/add-auth"}
            },
            "repository": {"full_name": "org/repo"}
        }"#;

        let event = handle_pull_request(payload.as_bytes()).unwrap().unwrap();
        assert_eq!(
            event.message.content,
            "@lexington opened PR #42: WIP: Add auth endpoint"
        );
        // Draft PRs should NOT trigger review spawn
        assert_eq!(event.needs_review, None);
    }

    #[test]
    fn test_handle_pull_request_opened_with_frontmatter() {
        let payload = r#"{
            "action": "opened",
            "number": 42,
            "pull_request": {
                "title": "Add auth endpoint",
                "user": {"login": "btucker"},
                "merged": false,
                "head": {"ref": "feature/something"},
                "body": "<!-- midtown: park -->\n\nSome description"
            },
            "repository": {"full_name": "org/repo"}
        }"#;

        let event = handle_pull_request(payload.as_bytes()).unwrap().unwrap();
        // Content includes @mention from frontmatter (takes priority over branch)
        assert_eq!(
            event.message.content,
            "@park opened PR #42: Add auth endpoint"
        );
        // Sender is always "github"
        assert_eq!(event.message.from, "github");
    }

    #[test]
    fn test_handle_pull_request_merged() {
        let payload = r#"{
            "action": "closed",
            "number": 42,
            "pull_request": {
                "title": "Add auth endpoint",
                "user": {"login": "btucker"},
                "merged": true,
                "head": {"ref": "lexington/add-auth"}
            },
            "repository": {"full_name": "org/repo"}
        }"#;

        let event = handle_pull_request(payload.as_bytes()).unwrap().unwrap();
        assert_eq!(
            event.message.content,
            "@lexington merged PR #42: Add auth endpoint"
        );
        assert_eq!(event.message.from, "github");
        // Merged PRs should NOT trigger review spawn
        assert_eq!(event.needs_review, None);
        // Merged PRs should flag for lead nudge
        assert_eq!(event.merged_pr, Some(42));
    }

    #[test]
    fn test_handle_pull_request_closed_not_merged() {
        let payload = r#"{
            "action": "closed",
            "number": 42,
            "pull_request": {
                "title": "Add auth endpoint",
                "user": {"login": "btucker"},
                "merged": false,
                "head": {"ref": "lexington/add-auth"}
            },
            "repository": {"full_name": "org/repo"}
        }"#;

        let event = handle_pull_request(payload.as_bytes()).unwrap().unwrap();
        assert_eq!(
            event.message.content,
            "@lexington closed PR #42 (not merged): Add auth endpoint"
        );
        // Closed (not merged) PRs should NOT flag for lead nudge
        assert_eq!(event.merged_pr, None);
    }

    #[test]
    fn test_handle_pull_request_no_coworker() {
        // When branch doesn't match a coworker, no @mention prefix
        let payload = r#"{
            "action": "opened",
            "number": 42,
            "pull_request": {
                "title": "Add auth endpoint",
                "user": {"login": "btucker"},
                "merged": false,
                "head": {"ref": "feature/something"}
            },
            "repository": {"full_name": "org/repo"}
        }"#;

        let event = handle_pull_request(payload.as_bytes()).unwrap().unwrap();
        // No @mention when no coworker is identified
        assert_eq!(event.message.content, "opened PR #42: Add auth endpoint");
        assert_eq!(event.message.from, "github");
        // Non-draft opened PR still triggers review even without coworker match
        assert_eq!(event.needs_review, Some(42));
    }

    #[test]
    fn test_handle_review_approved() {
        let payload = r#"{
            "action": "submitted",
            "review": {
                "id": 100,
                "state": "approved",
                "user": {"login": "madison"}
            },
            "pull_request": {"number": 42},
            "repository": {"full_name": "org/repo"}
        }"#;

        let event = handle_pull_request_review(payload.as_bytes())
            .unwrap()
            .unwrap();
        assert_eq!(event.message.content, "madison approved PR #42");
        // Should include review node for reactions
        let activity = event.pr_activity.unwrap();
        assert!(matches!(
            activity.comment_node,
            Some(CommentNode::Review {
                pull: 42,
                review_id: 100
            })
        ));
        assert_eq!(activity.repo_full_name.as_deref(), Some("org/repo"));
    }

    #[test]
    fn test_handle_ci_status() {
        let payload = r#"{
            "state": "success",
            "context": "ci/tests",
            "description": "All tests passed",
            "sha": "abc123",
            "repository": {"full_name": "org/repo"}
        }"#;

        let event = handle_status(payload.as_bytes()).unwrap().unwrap();
        assert_eq!(
            event.message.content,
            "CI passed (ci/tests): All tests passed"
        );
    }

    #[test]
    fn test_ignores_pending_status() {
        let payload = r#"{
            "state": "pending",
            "context": "ci/tests",
            "description": "Running",
            "sha": "abc123",
            "repository": {"full_name": "org/repo"}
        }"#;

        let event = handle_status(payload.as_bytes()).unwrap();
        assert!(event.is_none());
    }

    #[test]
    fn test_handle_review_with_branch_attribution() {
        let payload = r#"{
            "action": "submitted",
            "review": {
                "id": 101,
                "state": "approved",
                "user": {"login": "btucker"}
            },
            "pull_request": {
                "number": 42,
                "head": {"ref": "amsterdam/fix-bug"},
                "body": "Some PR description"
            },
            "repository": {"full_name": "org/repo"}
        }"#;

        let event = handle_pull_request_review(payload.as_bytes())
            .unwrap()
            .unwrap();
        // Content includes @mention prefix for coworker from branch
        assert_eq!(event.message.content, "@amsterdam btucker approved PR #42");
        // Sender is always "github"
        assert_eq!(event.message.from, "github");
    }

    #[test]
    fn test_handle_review_with_frontmatter_attribution() {
        let payload = r#"{
            "action": "submitted",
            "review": {
                "id": 102,
                "state": "changes_requested",
                "user": {"login": "reviewer"}
            },
            "pull_request": {
                "number": 55,
                "head": {"ref": "feature/unrelated"},
                "body": "<!-- midtown: columbus -->\n\nSome description"
            },
            "repository": {"full_name": "org/repo"}
        }"#;

        let event = handle_pull_request_review(payload.as_bytes())
            .unwrap()
            .unwrap();
        // Frontmatter takes priority for @mention
        assert_eq!(
            event.message.content,
            "@columbus reviewer requested changes on PR #55"
        );
        // Sender is always "github"
        assert_eq!(event.message.from, "github");
    }

    #[test]
    fn test_handle_ci_status_with_branch_attribution() {
        let payload = r#"{
            "state": "failure",
            "context": "ci/tests",
            "description": "Tests failed",
            "sha": "abc123",
            "branches": [{"name": "riverside/add-feature"}],
            "repository": {"full_name": "org/repo"}
        }"#;

        let event = handle_status(payload.as_bytes()).unwrap().unwrap();
        // Content includes @mention prefix for coworker from branch
        assert_eq!(
            event.message.content,
            "@riverside CI failed (ci/tests): Tests failed"
        );
        // Sender is always "github"
        assert_eq!(event.message.from, "github");
    }

    #[test]
    fn test_handle_check_run_with_branch_attribution() {
        let payload = r#"{
            "action": "completed",
            "check_run": {
                "name": "build",
                "status": "completed",
                "conclusion": "success",
                "check_suite": {
                    "head_sha": "abc123",
                    "head_branch": "park/implement-thing",
                    "pull_requests": [{"number": 99}]
                }
            },
            "repository": {"full_name": "org/repo"}
        }"#;

        let event = handle_check_run(payload.as_bytes()).unwrap().unwrap();
        // Content includes @mention prefix for coworker from branch
        assert_eq!(
            event.message.content,
            "@park Check 'build' passed on PR #99"
        );
        // Sender is always "github"
        assert_eq!(event.message.from, "github");
    }

    #[test]
    fn test_handle_check_run_on_main_branch() {
        let payload = r#"{
            "action": "completed",
            "check_run": {
                "name": "build",
                "status": "completed",
                "conclusion": "success",
                "check_suite": {
                    "head_sha": "abc123",
                    "head_branch": "main",
                    "pull_requests": []
                }
            },
            "repository": {"full_name": "org/repo"}
        }"#;

        let event = handle_check_run(payload.as_bytes()).unwrap().unwrap();
        // No PR, so shows branch name instead
        assert_eq!(event.message.content, "Check 'build' passed on main");
        assert_eq!(event.message.from, "github");
    }

    #[test]
    fn test_handle_check_run_failure_on_default_branch_nudges_lead() {
        let payload = r#"{
            "action": "completed",
            "check_run": {
                "name": "build",
                "status": "completed",
                "conclusion": "failure",
                "check_suite": {
                    "head_sha": "abc123",
                    "head_branch": "main",
                    "pull_requests": []
                }
            },
            "repository": {"full_name": "org/repo", "default_branch": "main"}
        }"#;

        let event = handle_check_run(payload.as_bytes()).unwrap().unwrap();
        assert_eq!(event.message.content, "Check 'build' failed on main");
        assert_eq!(
            event.ci_failed_on_default_branch.as_deref(),
            Some("@lead CI check 'build' failed on main — investigate ASAP")
        );
    }

    #[test]
    fn test_handle_check_run_failure_on_pr_branch_no_nudge() {
        let payload = r#"{
            "action": "completed",
            "check_run": {
                "name": "build",
                "status": "completed",
                "conclusion": "failure",
                "check_suite": {
                    "head_sha": "abc123",
                    "head_branch": "park/implement-thing",
                    "pull_requests": [{"number": 99}]
                }
            },
            "repository": {"full_name": "org/repo", "default_branch": "main"}
        }"#;

        let event = handle_check_run(payload.as_bytes()).unwrap().unwrap();
        assert_eq!(
            event.message.content,
            "@park Check 'build' failed on PR #99"
        );
        assert!(event.ci_failed_on_default_branch.is_none());
    }

    #[test]
    fn test_handle_check_run_success_on_default_branch_no_nudge() {
        let payload = r#"{
            "action": "completed",
            "check_run": {
                "name": "build",
                "status": "completed",
                "conclusion": "success",
                "check_suite": {
                    "head_sha": "abc123",
                    "head_branch": "main",
                    "pull_requests": []
                }
            },
            "repository": {"full_name": "org/repo", "default_branch": "main"}
        }"#;

        let event = handle_check_run(payload.as_bytes()).unwrap().unwrap();
        assert_eq!(event.message.content, "Check 'build' passed on main");
        assert!(event.ci_failed_on_default_branch.is_none());
    }

    #[test]
    fn test_handle_check_run_timed_out_on_default_branch_nudges_lead() {
        let payload = r#"{
            "action": "completed",
            "check_run": {
                "name": "E2E Tests",
                "status": "completed",
                "conclusion": "timed_out",
                "check_suite": {
                    "head_sha": "abc123",
                    "head_branch": "master",
                    "pull_requests": []
                }
            },
            "repository": {"full_name": "org/repo", "default_branch": "master"}
        }"#;

        let event = handle_check_run(payload.as_bytes()).unwrap().unwrap();
        assert_eq!(
            event.message.content,
            "Check 'E2E Tests' timed out on master"
        );
        assert_eq!(
            event.ci_failed_on_default_branch.as_deref(),
            Some("@lead CI check 'E2E Tests' failed on master — investigate ASAP")
        );
    }

    #[test]
    fn test_handle_check_run_failure_on_non_default_branch_no_pr_no_nudge() {
        // A branch that's not the default and has no PR — no nudge
        let payload = r#"{
            "action": "completed",
            "check_run": {
                "name": "build",
                "status": "completed",
                "conclusion": "failure",
                "check_suite": {
                    "head_sha": "abc123",
                    "head_branch": "feature/experiment",
                    "pull_requests": []
                }
            },
            "repository": {"full_name": "org/repo", "default_branch": "main"}
        }"#;

        let event = handle_check_run(payload.as_bytes()).unwrap().unwrap();
        assert_eq!(
            event.message.content,
            "Check 'build' failed on feature/experiment"
        );
        assert!(event.ci_failed_on_default_branch.is_none());
    }

    #[test]
    fn test_handle_review_comment_with_branch_attribution() {
        let payload = r#"{
            "action": "created",
            "pull_request": {
                "number": 77,
                "head": {"ref": "madison/refactor"},
                "body": "PR body here"
            },
            "comment": {
                "id": 200,
                "user": {"login": "reviewer"},
                "body": "Nice work!"
            },
            "repository": {"full_name": "org/repo"}
        }"#;

        let event = handle_review_comment(payload.as_bytes()).unwrap().unwrap();
        // Content includes @mention prefix for coworker from branch
        assert_eq!(
            event.message.content,
            "@madison reviewer left review comment on PR #77: Nice work!"
        );
        // Sender is always "github"
        assert_eq!(event.message.from, "github");
        // PR activity should identify madison as owner
        let activity = event.pr_activity.unwrap();
        assert_eq!(activity.pr_number, 77);
        assert_eq!(activity.owner_coworker.as_deref(), Some("madison"));
        assert_eq!(activity.actor, "reviewer");
        // Should include comment node for reactions
        assert!(matches!(
            activity.comment_node,
            Some(CommentNode::ReviewComment(200))
        ));
        assert_eq!(activity.repo_full_name.as_deref(), Some("org/repo"));
    }

    #[test]
    fn test_handle_issue_comment_with_coworker_signature() {
        // When a coworker posts a comment with <!-- midtown: name --> signature,
        // use the coworker name instead of GitHub username
        let payload = r#"{
            "action": "created",
            "issue": {
                "number": 42,
                "pull_request": {}
            },
            "comment": {
                "id": 201,
                "user": {"login": "btucker"},
                "body": "<!-- midtown: columbus -->\n\nLGTM! Nice fix."
            },
            "repository": {"full_name": "org/repo"}
        }"#;

        let event = handle_issue_comment(payload.as_bytes()).unwrap().unwrap();
        // Should use coworker name from signature, not GitHub username
        assert_eq!(
            event.message.content,
            "columbus commented on PR #42: LGTM! Nice fix."
        );
        assert_eq!(event.message.from, "github");
        // PR activity should identify commenter
        let activity = event.pr_activity.unwrap();
        assert_eq!(activity.pr_number, 42);
        assert_eq!(activity.actor, "columbus");
        // Should include comment node for reactions
        assert!(matches!(
            activity.comment_node,
            Some(CommentNode::IssueComment(201))
        ));
        assert_eq!(activity.repo_full_name.as_deref(), Some("org/repo"));
    }

    #[test]
    fn test_handle_issue_comment_without_signature() {
        // When no coworker signature, use the GitHub username as before
        let payload = r#"{
            "action": "created",
            "issue": {
                "number": 42,
                "pull_request": {}
            },
            "comment": {
                "id": 202,
                "user": {"login": "btucker"},
                "body": "Regular comment without signature"
            },
            "repository": {"full_name": "org/repo"}
        }"#;

        let event = handle_issue_comment(payload.as_bytes()).unwrap().unwrap();
        // Should use GitHub username when no signature
        assert_eq!(
            event.message.content,
            "btucker commented on PR #42: Regular comment without signature"
        );
        assert_eq!(event.message.from, "github");
    }

    #[test]
    fn test_handle_review_comment_with_coworker_signature() {
        // When a coworker posts a review comment with signature,
        // use the coworker name instead of GitHub username
        let payload = r#"{
            "action": "created",
            "pull_request": {
                "number": 77,
                "head": {"ref": "madison/refactor"},
                "body": "PR body here"
            },
            "comment": {
                "id": 203,
                "user": {"login": "btucker"},
                "body": "<!-- midtown: lexington -->\n\nConsider using a match here."
            },
            "repository": {"full_name": "org/repo"}
        }"#;

        let event = handle_review_comment(payload.as_bytes()).unwrap().unwrap();
        // Should use coworker name from comment signature
        // Note: @mention still uses PR attribution (madison), but commenter is lexington
        assert_eq!(
            event.message.content,
            "@madison lexington left review comment on PR #77: Consider using a match here."
        );
        assert_eq!(event.message.from, "github");
        // PR activity should identify madison as owner and lexington as actor
        let activity = event.pr_activity.unwrap();
        assert_eq!(activity.pr_number, 77);
        assert_eq!(activity.owner_coworker.as_deref(), Some("madison"));
        assert_eq!(activity.actor, "lexington");
    }

    #[test]
    fn test_handle_review_comment_without_signature_uses_branch() {
        // When no coworker signature in the comment, but the PR branch
        // maps to a coworker and the commenter is the repo owner (shared account),
        // use the branch-derived coworker name
        let payload = r#"{
            "action": "created",
            "pull_request": {
                "number": 77,
                "head": {"ref": "madison/refactor"},
                "body": "<!-- midtown: madison -->\n\nPR body"
            },
            "comment": {
                "id": 204,
                "user": {"login": "btucker"},
                "body": "Good point, I'll fix that."
            },
            "repository": {"full_name": "btucker/midtown"}
        }"#;

        let event = handle_review_comment(payload.as_bytes()).unwrap().unwrap();
        // Should use coworker name from PR branch/body, not GitHub username
        assert_eq!(
            event.message.content,
            "@madison madison left review comment on PR #77: Good point, I'll fix that."
        );
        // Actor should be the coworker, not the GitHub username
        let activity = event.pr_activity.unwrap();
        assert_eq!(activity.actor, "madison");
    }

    #[test]
    fn test_handle_review_comment_external_user_keeps_username() {
        // When the commenter is NOT the repo owner (e.g., an external reviewer),
        // keep the GitHub username even without frontmatter
        let payload = r#"{
            "action": "created",
            "pull_request": {
                "number": 77,
                "head": {"ref": "madison/refactor"},
                "body": "PR body here"
            },
            "comment": {
                "id": 205,
                "user": {"login": "external_reviewer"},
                "body": "Nice work!"
            },
            "repository": {"full_name": "btucker/midtown"}
        }"#;

        let event = handle_review_comment(payload.as_bytes()).unwrap().unwrap();
        // Should keep the external user's GitHub username
        assert_eq!(
            event.message.content,
            "@madison external_reviewer left review comment on PR #77: Nice work!"
        );
        let activity = event.pr_activity.unwrap();
        assert_eq!(activity.actor, "external_reviewer");
    }

    #[test]
    fn test_review_approved_produces_state_change() {
        let payload = r#"{
            "action": "submitted",
            "review": {
                "id": 200,
                "state": "approved",
                "user": {"login": "reviewer_bot"}
            },
            "pull_request": {
                "number": 42,
                "head": {"ref": "broadway/fix-bug"},
                "body": "Some PR description"
            },
            "repository": {"full_name": "org/repo"}
        }"#;

        let event = handle_pull_request_review(payload.as_bytes())
            .unwrap()
            .unwrap();
        let change = event.review_state_change.unwrap();
        assert_eq!(change.pr_number, 42);
        assert_eq!(change.owner_coworker.as_deref(), Some("broadway"));
        assert_eq!(change.reviewer, "reviewer_bot");
        assert_eq!(change.state, ReviewState::Approved);
        // CI failure should be None for review events
        assert!(event.pr_ci_failure.is_none());
    }

    #[test]
    fn test_review_changes_requested_produces_state_change() {
        let payload = r#"{
            "action": "submitted",
            "review": {
                "id": 201,
                "state": "changes_requested",
                "user": {"login": "reviewer_bot"}
            },
            "pull_request": {
                "number": 55,
                "head": {"ref": "columbus/add-feature"},
                "body": "<!-- midtown: columbus -->\n\nDescription"
            },
            "repository": {"full_name": "org/repo"}
        }"#;

        let event = handle_pull_request_review(payload.as_bytes())
            .unwrap()
            .unwrap();
        let change = event.review_state_change.unwrap();
        assert_eq!(change.pr_number, 55);
        assert_eq!(change.owner_coworker.as_deref(), Some("columbus"));
        assert_eq!(change.reviewer, "reviewer_bot");
        assert_eq!(change.state, ReviewState::ChangesRequested);
    }

    #[test]
    fn test_review_commented_no_state_change() {
        let payload = r#"{
            "action": "submitted",
            "review": {
                "id": 202,
                "state": "commented",
                "user": {"login": "reviewer_bot"}
            },
            "pull_request": {
                "number": 42,
                "head": {"ref": "broadway/fix-bug"}
            },
            "repository": {"full_name": "org/repo"}
        }"#;

        let event = handle_pull_request_review(payload.as_bytes())
            .unwrap()
            .unwrap();
        // "commented" reviews should NOT produce a state change
        assert!(event.review_state_change.is_none());
    }

    #[test]
    fn test_check_run_failure_on_pr_produces_ci_failure() {
        let payload = r#"{
            "action": "completed",
            "check_run": {
                "name": "Build",
                "status": "completed",
                "conclusion": "failure",
                "check_suite": {
                    "head_sha": "abc123",
                    "head_branch": "park/implement-thing",
                    "pull_requests": [{"number": 99}]
                }
            },
            "repository": {"full_name": "org/repo", "default_branch": "main"}
        }"#;

        let event = handle_check_run(payload.as_bytes()).unwrap().unwrap();
        let failure = event.pr_ci_failure.unwrap();
        assert_eq!(failure.pr_number, 99);
        assert_eq!(failure.owner_coworker.as_deref(), Some("park"));
        assert_eq!(failure.check_name, "Build");
        // Should NOT flag as default-branch CI failure
        assert!(event.ci_failed_on_default_branch.is_none());
    }

    #[test]
    fn test_check_run_success_on_pr_no_ci_failure() {
        let payload = r#"{
            "action": "completed",
            "check_run": {
                "name": "Build",
                "status": "completed",
                "conclusion": "success",
                "check_suite": {
                    "head_sha": "abc123",
                    "head_branch": "park/implement-thing",
                    "pull_requests": [{"number": 99}]
                }
            },
            "repository": {"full_name": "org/repo", "default_branch": "main"}
        }"#;

        let event = handle_check_run(payload.as_bytes()).unwrap().unwrap();
        assert!(event.pr_ci_failure.is_none());
    }

    #[test]
    fn test_check_run_failure_on_main_no_pr_ci_failure() {
        let payload = r#"{
            "action": "completed",
            "check_run": {
                "name": "Build",
                "status": "completed",
                "conclusion": "failure",
                "check_suite": {
                    "head_sha": "abc123",
                    "head_branch": "main",
                    "pull_requests": []
                }
            },
            "repository": {"full_name": "org/repo", "default_branch": "main"}
        }"#;

        let event = handle_check_run(payload.as_bytes()).unwrap().unwrap();
        // No PR associated → no pr_ci_failure
        assert!(event.pr_ci_failure.is_none());
        // But it should flag as default-branch CI failure
        assert!(event.ci_failed_on_default_branch.is_some());
    }

    #[test]
    fn test_pull_request_opened_no_review_state_change() {
        let payload = r#"{
            "action": "opened",
            "number": 42,
            "pull_request": {
                "title": "Add feature",
                "user": {"login": "btucker"},
                "merged": false,
                "head": {"ref": "broadway/add-feature"}
            },
            "repository": {"full_name": "org/repo"}
        }"#;

        let event = handle_pull_request(payload.as_bytes()).unwrap().unwrap();
        // PR opened events should not produce review state changes or CI failures
        assert!(event.review_state_change.is_none());
        assert!(event.pr_ci_failure.is_none());
    }

    // -------------------------------------------------------------------------
    // is_review_comment — detects Claude reviews from webhook payloads
    // -------------------------------------------------------------------------

    #[test]
    fn test_is_review_comment_detects_midtown_marker() {
        // Coworkers include <!-- midtown: name --> markers in their reviews
        assert!(is_review_comment(
            "<!-- midtown: park -->\n\n## Code Review"
        ));
        assert!(is_review_comment(
            "<!-- midtown:reviewer=lexington -->\n\nLGTM!"
        ));
    }

    #[test]
    fn test_is_review_comment_detects_emoji_signature() {
        assert!(is_review_comment(
            "## Summary\n\nLGTM!\n\n🤖 Reviewed by amsterdam"
        ));
    }

    #[test]
    fn test_is_review_comment_detects_code_review_header() {
        assert!(is_review_comment(
            "## Code Review by columbus\n\nLooks good!"
        ));
    }

    #[test]
    fn test_is_review_comment_detects_no_issues_found() {
        assert!(is_review_comment("## No Issues Found\n\nCode looks clean."));
    }

    #[test]
    fn test_is_review_comment_returns_false_for_normal_comment() {
        assert!(!is_review_comment("Thanks for the PR! I'll take a look."));
        assert!(!is_review_comment("Can you add some tests for this?"));
    }

    #[test]
    fn test_compute_check_duration_valid_timestamps() {
        let result = compute_check_duration(
            Some("2026-02-04T12:00:00Z"),
            Some("2026-02-04T12:05:30Z"),
            "Test",
        );
        let duration = result.expect("should return duration for valid timestamps");
        assert_eq!(duration.check_name, "Test");
        assert_eq!(duration.duration_secs, 330); // 5 minutes 30 seconds
    }

    #[test]
    fn test_compute_check_duration_missing_started_at() {
        let result = compute_check_duration(None, Some("2026-02-04T12:05:30Z"), "Test");
        assert!(
            result.is_none(),
            "should return None when started_at is missing"
        );
    }

    #[test]
    fn test_compute_check_duration_missing_completed_at() {
        let result = compute_check_duration(Some("2026-02-04T12:00:00Z"), None, "Test");
        assert!(
            result.is_none(),
            "should return None when completed_at is missing"
        );
    }

    #[test]
    fn test_compute_check_duration_invalid_started_at() {
        let result = compute_check_duration(
            Some("not-a-timestamp"),
            Some("2026-02-04T12:05:30Z"),
            "Test",
        );
        assert!(
            result.is_none(),
            "should return None for invalid started_at"
        );
    }

    #[test]
    fn test_compute_check_duration_invalid_completed_at() {
        let result = compute_check_duration(Some("2026-02-04T12:00:00Z"), Some("invalid"), "Test");
        assert!(
            result.is_none(),
            "should return None for invalid completed_at"
        );
    }

    #[test]
    fn test_compute_check_duration_reversed_timestamps_clamps_to_zero() {
        // completed_at before started_at - should clamp to 0
        let result = compute_check_duration(
            Some("2026-02-04T12:10:00Z"),
            Some("2026-02-04T12:00:00Z"),
            "Test",
        );
        let duration = result.expect("should return duration even for reversed timestamps");
        assert_eq!(
            duration.duration_secs, 0,
            "negative duration should clamp to 0"
        );
    }

    #[test]
    fn test_compute_check_duration_over_24_hours_returns_none() {
        // 25 hours = 90000 seconds, exceeds 86400 limit
        let result = compute_check_duration(
            Some("2026-02-03T11:00:00Z"),
            Some("2026-02-04T12:00:00Z"),
            "Test",
        );
        assert!(
            result.is_none(),
            "should return None for durations over 24 hours"
        );
    }

    #[test]
    fn test_compute_check_duration_exactly_24_hours_returns_none() {
        // exactly 86400 seconds - should be rejected (> check is strict)
        let result = compute_check_duration(
            Some("2026-02-03T12:00:00Z"),
            Some("2026-02-04T12:00:01Z"),
            "Test",
        );
        assert!(result.is_none(), "should return None for duration > 86400s");
    }

    #[test]
    fn test_compute_check_duration_just_under_24_hours_is_valid() {
        // 86399 seconds - should be accepted
        let result = compute_check_duration(
            Some("2026-02-03T12:00:01Z"),
            Some("2026-02-04T12:00:00Z"),
            "Test",
        );
        let duration = result.expect("should accept duration just under 24 hours");
        assert_eq!(duration.duration_secs, 86399);
    }
}
