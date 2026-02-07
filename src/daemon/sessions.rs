//! Session manager for headless coworker processes.
//!
//! `SessionManager` owns running `HeadlessSession` instances and provides the
//! daemon with spawn/nudge/shutdown/health primitives. It replaces the tmux-based
//! coworker process management for headless execution.
//!
//! The manager runs within the daemon's async runtime. Each coworker session is
//! a child process communicating via stdin/stdout JSON streams.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::headless::{HeadlessConfig, HeadlessSession, StreamEvent};

/// Status of a managed headless session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum SessionStatus {
    /// Session is starting up (spawned, waiting for init event).
    Starting,
    /// Session is running normally.
    Running,
    /// Session has exited or been killed.
    Stopped,
}

/// A managed coworker session with metadata.
#[allow(dead_code)]
pub struct CoworkerSession {
    /// The running headless session (None if stopped).
    session: Option<HeadlessSession>,
    /// Coworker name.
    pub name: String,
    /// Current session status.
    pub status: SessionStatus,
    /// When the session was spawned.
    pub started_at: DateTime<Utc>,
    /// Claude Code session ID (from init event, for resume).
    pub session_id: Option<String>,
    /// Cumulative cost in USD.
    pub cost_usd: f64,
    /// Last time we received an event from the session.
    pub last_event_at: Option<DateTime<Utc>>,
    /// Whether the session has hit a usage limit.
    pub has_usage_limit: bool,
    /// Whether the session has an API error.
    pub has_api_error: bool,
    /// Whether the session has a running subagent.
    pub has_running_subagent: bool,
}

impl CoworkerSession {
    fn new(name: String, session: HeadlessSession) -> Self {
        Self {
            session: Some(session),
            name,
            status: SessionStatus::Starting,
            started_at: Utc::now(),
            session_id: None,
            cost_usd: 0.0,
            last_event_at: None,
            has_usage_limit: false,
            has_api_error: false,
            has_running_subagent: false,
        }
    }
}

/// Manages running headless sessions for coworkers.
///
/// Thread-safe: uses `RwLock` for concurrent access from the daemon's
/// event loop, RPC handlers, and health checks.
#[allow(dead_code)]
pub struct SessionManager {
    sessions: RwLock<HashMap<String, CoworkerSession>>,
}

#[allow(dead_code)]
impl SessionManager {
    /// Create a new empty session manager.
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// Spawn a new headless session for a coworker.
    ///
    /// The `config` must have `cwd` set to the coworker's worktree path.
    /// If `initial_prompt` is provided, it's sent as the first user message.
    ///
    /// Returns the coworker name on success.
    pub async fn spawn(
        &self,
        name: &str,
        config: &HeadlessConfig,
        initial_prompt: Option<&str>,
    ) -> Result<(), crate::Error> {
        // Check for duplicate
        {
            let sessions = self.sessions.read().await;
            if sessions.contains_key(name) {
                return Err(crate::Error::Rpc {
                    code: -32603,
                    message: format!("Headless session '{}' already exists", name),
                });
            }
        }

        // Spawn the headless process
        let mut session = HeadlessSession::spawn(config).map_err(|e| crate::Error::Rpc {
            code: -32603,
            message: format!("Failed to spawn headless session for '{}': {}", name, e),
        })?;

        // Send initial prompt if provided
        if let Some(prompt) = initial_prompt
            && let Err(e) = session.send_message(prompt).await
        {
            warn!(
                "Failed to send initial prompt to '{}': {} — session still running",
                name, e
            );
        }

        let mut sessions = self.sessions.write().await;
        sessions.insert(
            name.to_string(),
            CoworkerSession::new(name.to_string(), session),
        );

        info!("Spawned headless session for '{}'", name);
        Ok(())
    }

    /// Send a message (nudge) to a running coworker session.
    ///
    /// This writes to the session's stdin via the stream-json input protocol.
    /// Unlike tmux send-keys, this doesn't require waiting for input stability.
    pub async fn send_message(&self, name: &str, message: &str) -> Result<(), crate::Error> {
        let mut sessions = self.sessions.write().await;
        let cs = sessions.get_mut(name).ok_or_else(|| crate::Error::Rpc {
            code: -32602,
            message: format!("No headless session for '{}'", name),
        })?;

        let session = cs.session.as_mut().ok_or_else(|| crate::Error::Rpc {
            code: -32603,
            message: format!("Session '{}' has stopped", name),
        })?;

        session
            .send_message(message)
            .await
            .map_err(|e| crate::Error::Rpc {
                code: -32603,
                message: format!("Failed to send message to '{}': {}", name, e),
            })?;

        debug!("Sent message to headless session '{}'", name);
        Ok(())
    }

    /// Shut down a coworker session.
    ///
    /// Kills the child process. The Claude Code session persists on disk
    /// (when `persist_session: true`) and can be resumed later.
    ///
    /// Returns the session ID (if known) for potential resume.
    pub async fn shutdown(&self, name: &str) -> Result<Option<String>, crate::Error> {
        let mut sessions = self.sessions.write().await;
        let cs = sessions.remove(name).ok_or_else(|| crate::Error::Rpc {
            code: -32602,
            message: format!("No headless session for '{}'", name),
        })?;

        let session_id = cs.session_id.clone();

        // The HeadlessSession's Drop impl will kill the child process
        drop(cs);

        info!(
            "Shut down headless session '{}' (session_id={:?})",
            name, session_id
        );
        Ok(session_id)
    }

    /// Check if a coworker has a running session.
    pub async fn is_alive(&self, name: &str) -> bool {
        let sessions = self.sessions.read().await;
        sessions
            .get(name)
            .is_some_and(|cs| cs.session.is_some() && cs.status != SessionStatus::Stopped)
    }

    /// Drain events from all sessions and update health state.
    ///
    /// Called periodically by the daemon tick. Reads available events from
    /// each session's stdout, updates session metadata (session_id, cost,
    /// health flags), and returns events for further processing.
    ///
    /// Also detects sessions that have exited and marks them as stopped.
    pub async fn drain_events(&self) -> HashMap<String, Vec<StreamEvent>> {
        let mut sessions = self.sessions.write().await;
        let mut all_events: HashMap<String, Vec<StreamEvent>> = HashMap::new();
        let mut stopped = Vec::new();

        for (name, cs) in sessions.iter_mut() {
            let session = match cs.session.as_mut() {
                Some(s) => s,
                None => continue,
            };

            let mut events = Vec::new();

            // Non-blocking drain: try to read events without waiting.
            // Use tokio::time::timeout with zero duration to poll.
            loop {
                match tokio::time::timeout(
                    std::time::Duration::from_millis(10),
                    session.next_event(),
                )
                .await
                {
                    Ok(Some(event)) => {
                        cs.last_event_at = Some(Utc::now());

                        match &event {
                            StreamEvent::System {
                                subtype,
                                session_id,
                                ..
                            } if subtype == "init" => {
                                cs.session_id = session_id.clone();
                                cs.status = SessionStatus::Running;
                                debug!(
                                    "Session '{}' initialized (session_id={:?})",
                                    name, cs.session_id
                                );
                            }
                            StreamEvent::Result {
                                total_cost_usd,
                                is_error,
                                ..
                            } => {
                                if let Some(cost) = total_cost_usd {
                                    cs.cost_usd = *cost;
                                }
                                if *is_error {
                                    cs.has_api_error = true;
                                }
                            }
                            StreamEvent::Assistant { message, .. } => {
                                // Check for subagent activity markers in assistant messages
                                if let Some(content) = message.get("content")
                                    && let Some(arr) = content.as_array()
                                {
                                    for block in arr {
                                        if block.get("type").and_then(|t| t.as_str())
                                            == Some("tool_use")
                                            && let Some(tool_name) =
                                                block.get("name").and_then(|n| n.as_str())
                                        {
                                            cs.has_running_subagent = tool_name == "Task"
                                                || tool_name == "dispatch_agent";
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }

                        events.push(event);
                    }
                    Ok(None) => {
                        // stdout closed — session has exited
                        cs.status = SessionStatus::Stopped;
                        cs.session = None;
                        stopped.push(name.clone());
                        debug!("Session '{}' exited (stdout closed)", name);
                        break;
                    }
                    Err(_) => {
                        // Timeout — no more events available right now
                        break;
                    }
                }
            }

            if !events.is_empty() {
                all_events.insert(name.clone(), events);
            }
        }

        all_events
    }

    /// Get the session ID for a coworker (if known).
    pub async fn get_session_id(&self, name: &str) -> Option<String> {
        let sessions = self.sessions.read().await;
        sessions.get(name).and_then(|cs| cs.session_id.clone())
    }

    /// Collect health data for all sessions.
    ///
    /// Returns a map of coworker name → ProcessHealth for the WorldSnapshot.
    pub async fn collect_health(&self) -> HashMap<String, super::snapshot::ProcessHealth> {
        let sessions = self.sessions.read().await;
        let mut health = HashMap::new();

        for (name, cs) in sessions.iter() {
            health.insert(
                name.clone(),
                super::snapshot::ProcessHealth {
                    is_alive: cs.session.is_some() && cs.status != SessionStatus::Stopped,
                    last_event_at: cs.last_event_at,
                    has_usage_limit: cs.has_usage_limit,
                    has_api_error: cs.has_api_error,
                    has_running_subagent: cs.has_running_subagent,
                    exit_code: None,
                },
            );
        }

        health
    }

    /// List all managed session names.
    pub async fn list_names(&self) -> Vec<String> {
        let sessions = self.sessions.read().await;
        sessions.keys().cloned().collect()
    }

    /// Remove a stopped session entry (cleanup after the coworker is fully shut down).
    pub async fn remove(&self, name: &str) {
        let mut sessions = self.sessions.write().await;
        sessions.remove(name);
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_manager_default() {
        let _sm = SessionManager::new();
    }

    #[tokio::test]
    async fn test_send_message_no_session() {
        let sm = SessionManager::new();
        let result = sm.send_message("nonexistent", "hello").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_shutdown_no_session() {
        let sm = SessionManager::new();
        let result = sm.shutdown("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_is_alive_no_session() {
        let sm = SessionManager::new();
        assert!(!sm.is_alive("nonexistent").await);
    }

    #[tokio::test]
    async fn test_drain_events_empty() {
        let sm = SessionManager::new();
        let events = sm.drain_events().await;
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn test_list_names_empty() {
        let sm = SessionManager::new();
        let names = sm.list_names().await;
        assert!(names.is_empty());
    }

    #[tokio::test]
    async fn test_collect_health_empty() {
        let sm = SessionManager::new();
        let health = sm.collect_health().await;
        assert!(health.is_empty());
    }
}
