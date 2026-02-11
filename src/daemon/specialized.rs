//! Specialized headless coworker abstraction.
//!
//! Provides a unified interface for headless coworkers with focused roles
//! (architect, clusterer, etc.) that aren't general-purpose developers.
//!
//! # Key Features
//!
//! - **Resume-on-demand lifecycle**: Fresh spawn or resume from session ID
//! - **Session persistence**: Session IDs stored in DaemonPersistentState
//! - **Model selection**: Role-specific (haiku/sonnet/opus) via config
//! - **Structured request/response**: Send context, receive typed response
//! - **Error handling**: Automatic retry on session corruption
//! - **Timeout management**: Per-request and inactivity timeouts
//!
//! # Design
//!
//! The abstraction is built around `SpecializedRole` — a trait defining the
//! behavior of each specialized coworker type. The `SpecializedCoworker` struct
//! manages session lifecycle (spawn/resume/retry) using `HeadlessSession` under
//! the hood.
//!
//! Unlike general-purpose coworkers (which run in worktrees and handle tasks),
//! specialized coworkers:
//! - Run in the main repo directory (no worktree isolation)
//! - Process structured requests (not free-form task assignments)
//! - Return typed responses (JSON, Mermaid diagrams, etc.)
//! - May be short-lived (one-shot like architect) or long-lived (resume-based like clusterer)
//!
//! # Future: Unified Coworker Model
//!
//! Long-term, all coworker types (dev, reviewer, architect, clusterer) should
//! implement a variant of `SpecializedRole` that defines their behavior
//! (system prompt, model, request/response patterns). The `SessionManager`
//! would then become a generic lifecycle manager that handles spawn/nudge/shutdown
//! for any role type.
//!
//! This requires:
//! - Extending `SpecializedRole` to support interactive (dev/reviewer) vs structured (architect/clusterer) modes
//! - Refactoring `SessionManager` to be role-agnostic
//! - Migrating dev/reviewer coworker logic to role implementations
//!
//! For now, this module focuses on structured-request roles (architect, clusterer)
//! as a proof of concept. Dev/reviewer integration is future work.
//!
//! # Example
//!
//! ```rust,ignore
//! // Define a role
//! struct ArchitectRole;
//!
//! impl SpecializedRole for ArchitectRole {
//!     type Request = InsightRequest;
//!     type Response = DiagramResponse;
//!
//!     fn role_name(&self) -> &'static str { "architect" }
//!     fn system_prompt(&self) -> String { /* ... */ }
//!     fn model(&self) -> &str { "sonnet" }
//!     fn persist_session(&self) -> bool { false }
//!     fn parse_response(&self, raw: &str) -> Result<Self::Response, String> { /* ... */ }
//! }
//!
//! // Execute a request
//! let role = ArchitectRole;
//! let request = InsightRequest { insight: "..." };
//! let result = SpecializedCoworker::execute(&role, request, None, timeout).await?;
//! ```

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::headless::{HeadlessConfig, HeadlessSession, StreamEvent};

/// Trait defining the behavior of a specialized coworker role.
///
/// Implementors specify:
/// - Role name (for logging and session tracking)
/// - System prompt
/// - Model preference (haiku/sonnet/opus)
/// - Whether to persist sessions (one-shot vs resume-on-demand)
/// - Request → Response transformation
pub trait SpecializedRole {
    /// Request type for this role (e.g., InsightRequest, ClusteringRequest).
    type Request;
    /// Response type for this role (e.g., DiagramResponse, ClusteringDiff).
    type Response;

    /// Human-readable role name (e.g., "architect", "clusterer").
    fn role_name(&self) -> &'static str;

    /// System prompt for this role's Claude session.
    fn system_prompt(&self) -> String;

    /// Model to use for this role (e.g., "haiku", "sonnet", "opus").
    fn model(&self) -> &str;

    /// Whether to persist sessions for resume. One-shot roles return false.
    fn persist_session(&self) -> bool;

    /// Maximum budget per request in USD. Defaults to $0.50.
    fn max_budget_usd(&self) -> f64 {
        0.50
    }

    /// Whether to allow tool use. Defaults to true (most specialized roles need tools).
    fn allow_tools(&self) -> bool {
        true
    }

    /// Timeout for a single request. Defaults to 2 minutes.
    fn request_timeout(&self) -> Duration {
        Duration::from_secs(120)
    }

    /// Format the request into a prompt string to send to the Claude session.
    fn format_request(&self, request: &Self::Request) -> String;

    /// Parse the raw text response from Claude into the typed response.
    ///
    /// Returns `Err` if the response is malformed or doesn't match expectations.
    fn parse_response(&self, raw: &str) -> Result<Self::Response, String>;
}

/// Result of executing a specialized coworker request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecializedResult<R> {
    /// The parsed response.
    pub response: R,
    /// Session ID (for resume-on-demand roles).
    pub session_id: Option<String>,
    /// Total API cost in USD.
    pub cost_usd: f64,
    /// Total duration in milliseconds.
    pub duration_ms: u64,
}

/// Error type for specialized coworker operations.
#[derive(Debug, thiserror::Error)]
pub enum SpecializedError {
    #[error("Headless session I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Response parsing failed: {0}")]
    ParseError(String),

    #[error("Session returned error")]
    SessionError,

    #[error("Session timed out after {0:?}")]
    Timeout(Duration),

    #[error("Session corruption detected, retry failed")]
    CorruptionRetryFailed,
}

/// Manages execution of specialized coworker requests.
///
/// This is a zero-sized struct — all methods are static/associated. The role trait
/// object carries the behavior, and session state is managed by the caller via
/// `session_id` parameter.
pub struct SpecializedCoworker;

impl SpecializedCoworker {
    /// Execute a request using the specified role.
    ///
    /// If `session_id` is provided and `persist_session()` returns true, resumes
    /// the existing session. Otherwise spawns a fresh session.
    ///
    /// On session corruption (e.g., "Tool names must be unique" error), automatically
    /// retries with a fresh session. The caller should update their session ID tracking
    /// to `result.session_id` after success.
    ///
    /// # Arguments
    ///
    /// * `role` - The specialized role defining behavior
    /// * `request` - The request to send
    /// * `session_id` - Optional existing session ID for resume
    /// * `cwd` - Working directory for the session. Defaults to current dir if None.
    /// * `timeout` - Request timeout override. Uses `role.request_timeout()` if None.
    ///
    /// # Returns
    ///
    /// `Ok(SpecializedResult)` on success, `Err(SpecializedError)` on failure.
    pub async fn execute<R>(
        role: &R,
        request: R::Request,
        session_id: Option<String>,
        cwd: Option<PathBuf>,
        timeout: Option<Duration>,
    ) -> Result<SpecializedResult<R::Response>, SpecializedError>
    where
        R: SpecializedRole,
    {
        let timeout = timeout.unwrap_or_else(|| role.request_timeout());
        let is_resume = session_id.is_some() && role.persist_session();

        info!(
            "{}: executing request (resume={}, timeout={}s)",
            role.role_name(),
            is_resume,
            timeout.as_secs()
        );

        // Attempt execution (may retry once on corruption)
        match Self::execute_inner(role, &request, session_id.clone(), cwd.clone(), timeout).await {
            Ok(result) => Ok(result),
            Err(SpecializedError::Io(ref e)) if is_resume && Self::is_corruption_error(e) => {
                warn!(
                    "{}: session corruption detected, retrying with fresh session",
                    role.role_name()
                );
                // Retry with fresh session (no session_id)
                Self::execute_inner(role, &request, None, cwd, timeout)
                    .await
                    .map_err(|_| SpecializedError::CorruptionRetryFailed)
            }
            Err(e) => Err(e),
        }
    }

    /// Inner execution logic (no retry on corruption).
    async fn execute_inner<R>(
        role: &R,
        request: &R::Request,
        session_id: Option<String>,
        cwd: Option<PathBuf>,
        timeout: Duration,
    ) -> Result<SpecializedResult<R::Response>, SpecializedError>
    where
        R: SpecializedRole,
    {
        let config = HeadlessConfig {
            model: role.model().to_string(),
            system_prompt: role.system_prompt(),
            json_schema: None,
            cwd: cwd.map(|p| p.to_string_lossy().to_string()),
            max_budget_usd: Some(role.max_budget_usd()),
            allow_tools: role.allow_tools(),
            persist_session: role.persist_session(),
            resume_session_id: session_id.clone(),
            inactivity_timeout: None,
            team_name: None,
            agent_id: None,
            agent_name: None,
            settings_path: None,
            setting_sources: None,
            env: std::collections::HashMap::new(),
        };

        let mut session = if let Some(ref sid) = session_id {
            if role.persist_session() {
                HeadlessSession::resume(sid, &config)?
            } else {
                HeadlessSession::spawn(&config)?
            }
        } else {
            HeadlessSession::spawn(&config)?
        };

        // Send the request
        let prompt = role.format_request(request);
        session.send_message(&prompt).await?;
        session.close_stdin();

        // Collect result with timeout
        let result =
            match tokio::time::timeout(timeout, Self::collect_result(role, &mut session)).await {
                Ok(Ok(res)) => res,
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    warn!(
                        "{}: request timed out after {}s",
                        role.role_name(),
                        timeout.as_secs()
                    );
                    return Err(SpecializedError::Timeout(timeout));
                }
            };

        info!(
            "{}: request complete (cost=${:.4}, duration={}ms)",
            role.role_name(),
            result.cost_usd,
            result.duration_ms
        );

        Ok(result)
    }

    /// Collect the final result from a headless session.
    async fn collect_result<R>(
        role: &R,
        session: &mut HeadlessSession,
    ) -> Result<SpecializedResult<R::Response>, SpecializedError>
    where
        R: SpecializedRole,
    {
        let mut result_text = None;
        let mut cost_usd = 0.0;
        let mut duration_ms = 0;
        let mut is_error = false;
        let mut session_id = None;

        while let Some(event) = session.next_event().await {
            match event {
                StreamEvent::System {
                    subtype,
                    session_id: sid,
                    ..
                } if subtype == "init" => {
                    session_id = sid;
                }
                StreamEvent::Result {
                    result,
                    total_cost_usd,
                    duration_ms: dur,
                    is_error: err,
                    session_id: sid,
                    ..
                } => {
                    result_text = result;
                    cost_usd = total_cost_usd.unwrap_or(0.0);
                    duration_ms = dur.unwrap_or(0);
                    is_error = err;
                    if session_id.is_none() {
                        session_id = sid;
                    }
                    break;
                }
                _ => {
                    // Skip intermediate events
                }
            }
        }

        // Wait for process to exit
        let _ = session.wait().await;

        if is_error {
            return Err(SpecializedError::SessionError);
        }

        let Some(raw) = result_text else {
            return Err(SpecializedError::ParseError(
                "No result text returned".to_string(),
            ));
        };

        // Parse the response using the role's parser
        let response = role
            .parse_response(&raw)
            .map_err(SpecializedError::ParseError)?;

        Ok(SpecializedResult {
            response,
            session_id,
            cost_usd,
            duration_ms,
        })
    }

    /// Check if an I/O error is a known session corruption error.
    ///
    /// Currently detects the "Tool names must be unique" error that occurs
    /// when resuming a session with conflicting MCP tool definitions.
    ///
    /// Note: For coworker sessions, the primary fix is in `headless.rs`
    /// (skip `--settings` on resume). Specialized sessions don't use
    /// `--settings`, so their variant of this error is intrinsic to
    /// Claude Code session resume — this retry-with-fresh-session
    /// workaround is still needed here.
    fn is_corruption_error(error: &std::io::Error) -> bool {
        let msg = error.to_string();
        msg.contains("Tool names must be unique")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test role for unit testing
    struct TestRole {
        model: String,
        persist: bool,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TestRequest {
        query: String,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TestResponse {
        answer: String,
    }

    impl SpecializedRole for TestRole {
        type Request = TestRequest;
        type Response = TestResponse;

        fn role_name(&self) -> &'static str {
            "test-role"
        }

        fn system_prompt(&self) -> String {
            "You are a test assistant.".to_string()
        }

        fn model(&self) -> &str {
            &self.model
        }

        fn persist_session(&self) -> bool {
            self.persist
        }

        fn format_request(&self, request: &Self::Request) -> String {
            format!("Query: {}", request.query)
        }

        fn parse_response(&self, raw: &str) -> Result<Self::Response, String> {
            if raw.trim().is_empty() {
                return Err("Empty response".to_string());
            }
            Ok(TestResponse {
                answer: raw.trim().to_string(),
            })
        }
    }

    #[test]
    fn test_role_trait_basics() {
        let role = TestRole {
            model: "haiku".to_string(),
            persist: false,
        };

        assert_eq!(role.role_name(), "test-role");
        assert_eq!(role.model(), "haiku");
        assert!(!role.persist_session());
        assert_eq!(role.max_budget_usd(), 0.50); // default
        assert!(role.allow_tools()); // default

        let request = TestRequest {
            query: "What is 2+2?".to_string(),
        };
        let formatted = role.format_request(&request);
        assert_eq!(formatted, "Query: What is 2+2?");

        let response = role.parse_response("4").unwrap();
        assert_eq!(response.answer, "4");

        let err = role.parse_response("").unwrap_err();
        assert_eq!(err, "Empty response");
    }

    #[test]
    fn test_role_with_custom_budget() {
        struct CustomBudgetRole;

        impl SpecializedRole for CustomBudgetRole {
            type Request = TestRequest;
            type Response = TestResponse;

            fn role_name(&self) -> &'static str {
                "custom-budget"
            }

            fn system_prompt(&self) -> String {
                "Test".to_string()
            }

            fn model(&self) -> &str {
                "opus"
            }

            fn persist_session(&self) -> bool {
                false
            }

            fn max_budget_usd(&self) -> f64 {
                1.0 // Override default
            }

            fn format_request(&self, _request: &Self::Request) -> String {
                "test".to_string()
            }

            fn parse_response(&self, raw: &str) -> Result<Self::Response, String> {
                Ok(TestResponse {
                    answer: raw.to_string(),
                })
            }
        }

        let role = CustomBudgetRole;
        assert_eq!(role.max_budget_usd(), 1.0);
    }

    #[test]
    fn test_is_corruption_error() {
        let normal_error = std::io::Error::other("Some error");
        assert!(!SpecializedCoworker::is_corruption_error(&normal_error));

        let corruption_error = std::io::Error::other("Tool names must be unique: duplicate 'foo'");
        assert!(SpecializedCoworker::is_corruption_error(&corruption_error));
    }

    #[test]
    fn test_specialized_result_serialization() {
        let result = SpecializedResult {
            response: TestResponse {
                answer: "42".to_string(),
            },
            session_id: Some("test-session".to_string()),
            cost_usd: 0.05,
            duration_ms: 1500,
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"answer\":\"42\""));
        assert!(json.contains("\"cost_usd\":0.05"));
        assert!(json.contains("\"duration_ms\":1500"));
    }

    #[test]
    fn test_specialized_error_display() {
        let err = SpecializedError::ParseError("Invalid JSON".to_string());
        assert_eq!(err.to_string(), "Response parsing failed: Invalid JSON");

        let err = SpecializedError::SessionError;
        assert_eq!(err.to_string(), "Session returned error");

        let err = SpecializedError::Timeout(Duration::from_secs(120));
        assert_eq!(err.to_string(), "Session timed out after 120s");

        let err = SpecializedError::CorruptionRetryFailed;
        assert_eq!(err.to_string(), "Session corruption detected, retry failed");
    }
}
