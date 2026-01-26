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
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
    Router,
};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::message::{Message, MessageType};

type HmacSha256 = Hmac<Sha256>;

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
    message_tx: mpsc::Sender<Message>,
}

/// GitHub webhook event header
const GITHUB_EVENT_HEADER: &str = "X-GitHub-Event";
/// GitHub signature header (SHA256)
const GITHUB_SIGNATURE_HEADER: &str = "X-Hub-Signature-256";

/// Start the webhook HTTP server
///
/// Returns a channel receiver for translated messages.
pub async fn start_webhook_server(
    config: WebhookConfig,
) -> crate::Result<mpsc::Receiver<Message>> {
    let (tx, rx) = mpsc::channel(100);

    let state = Arc::new(WebhookState {
        config: config.clone(),
        message_tx: tx,
    });

    let app = Router::new()
        .route("/webhook", post(handle_webhook))
        .route("/health", axum::routing::get(health_check))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    info!("Starting webhook server on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            error!("Webhook server error: {}", e);
        }
    });

    Ok(rx)
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
    let message = match event_type {
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

    match message {
        Ok(Some(msg)) => {
            if let Err(e) = state.message_tx.send(msg).await {
                error!("Failed to send message: {}", e);
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
    user: User,
    merged: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct PullRequestReviewEvent {
    action: String,
    review: Review,
    pull_request: PullRequestRef,
    #[allow(dead_code)]
    repository: Repository,
}

#[derive(Debug, Deserialize)]
struct Review {
    state: String,
    user: User,
}

#[derive(Debug, Deserialize)]
struct PullRequestRef {
    number: u64,
}

#[derive(Debug, Deserialize)]
struct IssueCommentEvent {
    action: String,
    issue: Issue,
    comment: Comment,
    #[allow(dead_code)]
    repository: Repository,
}

#[derive(Debug, Deserialize)]
struct Issue {
    number: u64,
    pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct Comment {
    user: User,
    body: String,
}

#[derive(Debug, Deserialize)]
struct ReviewCommentEvent {
    action: String,
    pull_request: PullRequestRef,
    comment: Comment,
    #[allow(dead_code)]
    repository: Repository,
}

#[derive(Debug, Deserialize)]
struct StatusEvent {
    state: String,
    context: String,
    description: Option<String>,
    #[allow(dead_code)]
    sha: String,
    #[allow(dead_code)]
    repository: Repository,
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
}

#[derive(Debug, Deserialize)]
struct CheckSuite {
    #[allow(dead_code)]
    head_sha: String,
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
}

// ============================================================================
// Event Handlers
// ============================================================================

fn handle_pull_request(body: &[u8]) -> Result<Option<Message>, serde_json::Error> {
    let event: PullRequestEvent = serde_json::from_slice(body)?;

    let content = match event.action.as_str() {
        "opened" => format!(
            "GitHub: {} opened PR #{}: {}",
            event.pull_request.user.login, event.number, event.pull_request.title
        ),
        "closed" => {
            if event.pull_request.merged.unwrap_or(false) {
                format!(
                    "GitHub: PR #{} merged: {}",
                    event.number, event.pull_request.title
                )
            } else {
                format!(
                    "GitHub: PR #{} closed (not merged): {}",
                    event.number, event.pull_request.title
                )
            }
        }
        "reopened" => format!(
            "GitHub: {} reopened PR #{}: {}",
            event.pull_request.user.login, event.number, event.pull_request.title
        ),
        "synchronize" => format!(
            "GitHub: PR #{} updated with new commits",
            event.number
        ),
        "ready_for_review" => format!(
            "GitHub: PR #{} is ready for review: {}",
            event.number, event.pull_request.title
        ),
        _ => return Ok(None),
    };

    Ok(Some(Message::new("github", content, MessageType::System)))
}

fn handle_pull_request_review(body: &[u8]) -> Result<Option<Message>, serde_json::Error> {
    let event: PullRequestReviewEvent = serde_json::from_slice(body)?;

    if event.action != "submitted" {
        return Ok(None);
    }

    let content = match event.review.state.to_lowercase().as_str() {
        "approved" => format!(
            "GitHub: {} approved PR #{}",
            event.review.user.login, event.pull_request.number
        ),
        "changes_requested" => format!(
            "GitHub: {} requested changes on PR #{}",
            event.review.user.login, event.pull_request.number
        ),
        "commented" => format!(
            "GitHub: {} commented on PR #{}",
            event.review.user.login, event.pull_request.number
        ),
        _ => return Ok(None),
    };

    Ok(Some(Message::new("github", content, MessageType::System)))
}

fn handle_issue_comment(body: &[u8]) -> Result<Option<Message>, serde_json::Error> {
    let event: IssueCommentEvent = serde_json::from_slice(body)?;

    if event.action != "created" {
        return Ok(None);
    }

    // Only handle PR comments, not issue comments
    if event.issue.pull_request.is_none() {
        return Ok(None);
    }

    let preview = truncate_comment(&event.comment.body, 50);
    let content = format!(
        "GitHub: {} commented on PR #{}: {}",
        event.comment.user.login, event.issue.number, preview
    );

    Ok(Some(Message::new("github", content, MessageType::System)))
}

fn handle_review_comment(body: &[u8]) -> Result<Option<Message>, serde_json::Error> {
    let event: ReviewCommentEvent = serde_json::from_slice(body)?;

    if event.action != "created" {
        return Ok(None);
    }

    let preview = truncate_comment(&event.comment.body, 50);
    let content = format!(
        "GitHub: {} left review comment on PR #{}: {}",
        event.comment.user.login, event.pull_request.number, preview
    );

    Ok(Some(Message::new("github", content, MessageType::System)))
}

fn handle_status(body: &[u8]) -> Result<Option<Message>, serde_json::Error> {
    let event: StatusEvent = serde_json::from_slice(body)?;

    let content = match event.state.as_str() {
        "success" => format!(
            "GitHub: CI passed ({}): {}",
            event.context,
            event.description.as_deref().unwrap_or("No description")
        ),
        "failure" => format!(
            "GitHub: CI failed ({}): {}",
            event.context,
            event.description.as_deref().unwrap_or("No description")
        ),
        "error" => format!(
            "GitHub: CI error ({}): {}",
            event.context,
            event.description.as_deref().unwrap_or("No description")
        ),
        "pending" => return Ok(None), // Don't spam pending statuses
        _ => return Ok(None),
    };

    Ok(Some(Message::new("github", content, MessageType::System)))
}

fn handle_check_run(body: &[u8]) -> Result<Option<Message>, serde_json::Error> {
    let event: CheckRunEvent = serde_json::from_slice(body)?;

    // Only report completed check runs
    if event.check_run.status != "completed" {
        return Ok(None);
    }

    let pr_info = event
        .check_run
        .check_suite
        .as_ref()
        .and_then(|cs| cs.pull_requests.first())
        .map(|pr| format!(" on PR #{}", pr.number))
        .unwrap_or_default();

    let content = match event.check_run.conclusion.as_deref() {
        Some("success") => format!(
            "GitHub: Check '{}' passed{}",
            event.check_run.name, pr_info
        ),
        Some("failure") => format!(
            "GitHub: Check '{}' failed{}",
            event.check_run.name, pr_info
        ),
        Some("cancelled") => format!(
            "GitHub: Check '{}' cancelled{}",
            event.check_run.name, pr_info
        ),
        Some("timed_out") => format!(
            "GitHub: Check '{}' timed out{}",
            event.check_run.name, pr_info
        ),
        _ => return Ok(None),
    };

    Ok(Some(Message::new("github", content, MessageType::System)))
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
        assert_eq!(truncate_comment("this is a longer comment", 10), "this is a ...");
        assert_eq!(
            truncate_comment("first line\nsecond line", 50),
            "first line"
        );
        // Test unicode safety - should not panic on multi-byte characters
        assert_eq!(truncate_comment("Hello 世界! More text here", 8), "Hello 世界...");
        assert_eq!(truncate_comment("emoji 👍 test", 7), "emoji 👍...");
    }

    #[test]
    fn test_handle_pull_request_opened() {
        let payload = r#"{
            "action": "opened",
            "number": 42,
            "pull_request": {
                "title": "Add auth endpoint",
                "user": {"login": "lexington"},
                "merged": false
            },
            "repository": {"full_name": "org/repo"}
        }"#;

        let msg = handle_pull_request(payload.as_bytes())
            .unwrap()
            .unwrap();
        assert_eq!(msg.content, "GitHub: lexington opened PR #42: Add auth endpoint");
        assert_eq!(msg.from, "github");
    }

    #[test]
    fn test_handle_pull_request_merged() {
        let payload = r#"{
            "action": "closed",
            "number": 42,
            "pull_request": {
                "title": "Add auth endpoint",
                "user": {"login": "lexington"},
                "merged": true
            },
            "repository": {"full_name": "org/repo"}
        }"#;

        let msg = handle_pull_request(payload.as_bytes())
            .unwrap()
            .unwrap();
        assert_eq!(msg.content, "GitHub: PR #42 merged: Add auth endpoint");
    }

    #[test]
    fn test_handle_review_approved() {
        let payload = r#"{
            "action": "submitted",
            "review": {
                "state": "approved",
                "user": {"login": "madison"}
            },
            "pull_request": {"number": 42},
            "repository": {"full_name": "org/repo"}
        }"#;

        let msg = handle_pull_request_review(payload.as_bytes())
            .unwrap()
            .unwrap();
        assert_eq!(msg.content, "GitHub: madison approved PR #42");
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

        let msg = handle_status(payload.as_bytes()).unwrap().unwrap();
        assert_eq!(msg.content, "GitHub: CI passed (ci/tests): All tests passed");
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

        let msg = handle_status(payload.as_bytes()).unwrap();
        assert!(msg.is_none());
    }
}
