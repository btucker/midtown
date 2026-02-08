//! Session manager for headless coworker processes.
//!
//! `SessionManager` owns running `HeadlessSession` instances and provides the
//! daemon with spawn/nudge/shutdown/health primitives. It replaces the tmux-based
//! coworker process management for headless execution.
//!
//! The manager runs within the daemon's async runtime. Each coworker session is
//! a child process communicating via stdin/stdout JSON streams.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

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
    /// Whether the session has a pending tool execution (tool_use seen, no tool_result yet).
    pub has_pending_tool: bool,
    /// File handle for writing stream events to JSONL log.
    /// Used for debugging and `midtown coworker view`.
    output_log: Option<std::fs::File>,
    /// Path to the output log file.
    output_log_path: PathBuf,
}

impl CoworkerSession {
    fn new(name: String, session: HeadlessSession, repo: &str) -> Self {
        let output_log_path = crate::paths::headless_output_file(repo, &name);

        // Open the log file in append mode, creating it if needed
        let output_log = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&output_log_path)
        {
            Ok(file) => Some(file),
            Err(e) => {
                warn!(
                    "Failed to open output log for '{}' at {:?}: {}",
                    name, output_log_path, e
                );
                None
            }
        };

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
            has_pending_tool: false,
            output_log,
            output_log_path,
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
    repo_name: String,
}

#[allow(dead_code)]
impl SessionManager {
    /// Create a new empty session manager.
    pub fn new(repo_name: String) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            repo_name,
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

        // Send initial prompt if provided — this is "Here's your mission",
        // so failure means the coworker has no task and would be non-functional.
        if let Some(prompt) = initial_prompt
            && let Err(e) = session.send_message(prompt).await
        {
            // Kill the orphaned session before returning the error
            let _ = session.kill().await;
            return Err(crate::Error::Rpc {
                code: -32603,
                message: format!(
                    "Failed to send initial prompt to '{}': {} — killed session",
                    name, e
                ),
            });
        }

        let mut sessions = self.sessions.write().await;
        sessions.insert(
            name.to_string(),
            CoworkerSession::new(name.to_string(), session, &self.repo_name),
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

    /// Shut down all coworker sessions.
    ///
    /// Called during daemon shutdown to prevent orphaned processes.
    /// Returns the number of sessions that were shut down.
    pub async fn shutdown_all(&self) -> usize {
        let mut sessions = self.sessions.write().await;
        let count = sessions.len();
        let names: Vec<String> = sessions.keys().cloned().collect();
        for name in &names {
            if let Some(cs) = sessions.remove(name) {
                let session_id = cs.session_id.clone();
                drop(cs); // Drop triggers process kill
                info!(
                    "Shut down headless session '{}' during daemon shutdown (session_id={:?})",
                    name, session_id
                );
            }
        }
        count
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
    /// health flags), and returns events grouped by coworker name.
    ///
    /// Also detects sessions that have exited: returns their names in the
    /// second tuple element along with stderr lines from the exited sessions
    /// in the third tuple element (as a HashMap<name, Vec<stderr_lines>>).
    pub async fn drain_events(
        &self,
    ) -> (
        HashMap<String, Vec<StreamEvent>>,
        Vec<String>,
        HashMap<String, Vec<String>>,
    ) {
        let mut sessions = self.sessions.write().await;
        let mut all_events: HashMap<String, Vec<StreamEvent>> = HashMap::new();
        let mut stopped = Vec::new();
        let mut stderr_by_name: HashMap<String, Vec<String>> = HashMap::new();
        // Collect (log_path, events) pairs for async writing after releasing the lock
        let mut events_to_log: Vec<(PathBuf, Vec<StreamEvent>)> = Vec::new();

        for (name, cs) in sessions.iter_mut() {
            let session = match cs.session.as_mut() {
                Some(s) => s,
                None => continue,
            };

            let mut events = Vec::new();

            // Drain stderr first to prevent pipe buffer deadlock.
            // If stderr writes >64KB without draining, the child process blocks.
            // This must happen every tick, not just on exit.
            let _ = session.drain_stderr().await;

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
                                // Check for subagent activity markers and pending tool state
                                if let Some(content) = message.get("content")
                                    && let Some(arr) = content.as_array()
                                {
                                    let mut has_tool_use = false;
                                    for block in arr {
                                        if block.get("type").and_then(|t| t.as_str())
                                            == Some("tool_use")
                                        {
                                            has_tool_use = true;
                                            if let Some(tool_name) =
                                                block.get("name").and_then(|n| n.as_str())
                                            {
                                                cs.has_running_subagent = tool_name == "Task"
                                                    || tool_name == "dispatch_agent";
                                            }
                                        }
                                    }
                                    // If we saw any tool_use, mark as pending (will be cleared by User event)
                                    if has_tool_use {
                                        cs.has_pending_tool = true;
                                    }
                                }
                            }
                            StreamEvent::User { message, .. } => {
                                // User events may contain tool_result blocks — only clear pending tool flag if present
                                if let Some(content) = message.get("content")
                                    && let Some(arr) = content.as_array()
                                {
                                    for block in arr {
                                        if block.get("type").and_then(|t| t.as_str())
                                            == Some("tool_result")
                                        {
                                            cs.has_pending_tool = false;
                                            break;
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
                        // Drain stderr before dropping the session
                        let stderr_lines = session.drain_stderr().await;
                        cs.status = SessionStatus::Stopped;
                        cs.session = None;
                        stopped.push(name.clone());

                        if !stderr_lines.is_empty() {
                            stderr_by_name.insert(name.clone(), stderr_lines.clone());
                            debug!(
                                "Session '{}' exited (stdout closed) with stderr: {:?}",
                                name, stderr_lines
                            );
                        } else {
                            debug!("Session '{}' exited (stdout closed)", name);
                        }
                        break;
                    }
                    Err(_) => {
                        // Timeout — no more events available right now
                        break;
                    }
                }
            }

            if !events.is_empty() {
                // Queue events for async logging (after releasing the lock)
                if cs.output_log.is_some() {
                    events_to_log.push((cs.output_log_path.clone(), events.clone()));
                }
                all_events.insert(name.clone(), events);
            }
        }

        // Release the lock before performing file I/O
        drop(sessions);

        // Write all collected events to their log files asynchronously
        // This keeps the drain loop fast and prevents stdout buffer blocking
        if !events_to_log.is_empty() {
            tokio::task::spawn_blocking(move || {
                for (log_path, events) in events_to_log {
                    if let Ok(mut file) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&log_path)
                    {
                        for event in events {
                            if let Ok(json) = serde_json::to_string(&event) {
                                let _ = writeln!(file, "{}", json);
                            }
                        }
                        // Flush once per session to support `tail -f`
                        let _ = file.flush();
                    }
                }
            });
        }

        (all_events, stopped, stderr_by_name)
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
                    has_pending_tool: cs.has_pending_tool,
                    exit_code: None,
                },
            );
        }

        health
    }

    /// List all managed session names (including stopped sessions pending cleanup).
    pub async fn list_names(&self) -> Vec<String> {
        let sessions = self.sessions.read().await;
        sessions.keys().cloned().collect()
    }

    /// List only alive session names (excludes stopped sessions pending cleanup).
    ///
    /// Use this instead of `list_names()` when building the headless preservation
    /// set for `sync_with_tmux()`. Using `list_names()` includes stopped sessions
    /// that haven't been removed yet, which can cause `sync_with_tmux` to preserve
    /// stale entries in the CoworkerManager tracking map.
    pub async fn list_alive_names(&self) -> Vec<String> {
        let sessions = self.sessions.read().await;
        sessions
            .iter()
            .filter(|(_, cs)| cs.status != SessionStatus::Stopped)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Remove a stopped session entry (cleanup after the coworker is fully shut down).
    pub async fn remove(&self, name: &str) {
        let log_path = {
            let mut sessions = self.sessions.write().await;
            let log_path = sessions.get(name).map(|cs| cs.output_log_path.clone());
            sessions.remove(name);
            log_path
        };

        // Delete the headless output log file if it exists
        if let Some(path) = log_path {
            tokio::task::spawn_blocking(move || {
                if let Err(e) = std::fs::remove_file(&path) {
                    debug!("Failed to remove output log {:?}: {}", path, e);
                }
            });
        }
    }

    /// Get recent output for a coworker from the JSONL log file.
    ///
    /// Reads the last ~200 lines from the headless output log and extracts
    /// text content from Assistant events. Returns None if the session doesn't
    /// exist or the log file can't be read.
    ///
    /// This enables `midtown coworker view` to work with headless coworkers.
    pub async fn get_output(&self, name: &str) -> Option<String> {
        // Get the log path without holding the lock during file I/O
        let log_path = {
            let sessions = self.sessions.read().await;
            let cs = sessions.get(name)?;
            cs.output_log_path.clone()
        };

        // Perform file I/O in spawn_blocking to avoid blocking the async runtime
        // (following pattern from commit 9575557)
        let content = tokio::task::spawn_blocking(move || std::fs::read_to_string(log_path))
            .await
            .ok()?
            .ok()?;

        if content.is_empty() {
            return Some(String::from("(no output yet)"));
        }

        // Collect last 200 lines
        let lines: Vec<String> = content
            .lines()
            .rev()
            .take(200)
            .map(|s| s.to_string())
            .collect();

        // Parse JSONL events and extract text from Assistant messages
        let mut output_lines = Vec::new();
        for line in lines.iter().rev() {
            if let Ok(StreamEvent::Assistant { message, .. }) =
                serde_json::from_str::<StreamEvent>(line)
                && let Some(content) = message.get("content")
                && let Some(arr) = content.as_array()
            {
                for block in arr {
                    if block.get("type").and_then(|t| t.as_str()) == Some("text")
                        && let Some(text) = block.get("text").and_then(|t| t.as_str())
                    {
                        output_lines.push(text.to_string());
                    }
                }
            }
        }

        Some(output_lines.join("\n"))
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new(String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Insert a fake session entry for testing (no real process).
    async fn insert_test_session(sm: &SessionManager, name: &str, status: SessionStatus) {
        let mut sessions = sm.sessions.write().await;
        sessions.insert(
            name.to_string(),
            CoworkerSession {
                session: None,
                name: name.to_string(),
                status,
                started_at: Utc::now(),
                session_id: None,
                cost_usd: 0.0,
                last_event_at: None,
                has_usage_limit: false,
                has_api_error: false,
                has_running_subagent: false,
                has_pending_tool: false,
                output_log: None,
                output_log_path: PathBuf::new(),
            },
        );
    }

    #[test]
    fn test_session_manager_default() {
        let _sm = SessionManager::new("test-repo".to_string());
    }

    #[tokio::test]
    async fn test_send_message_no_session() {
        let sm = SessionManager::new("test-repo".to_string());
        let result = sm.send_message("nonexistent", "hello").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_shutdown_no_session() {
        let sm = SessionManager::new("test-repo".to_string());
        let result = sm.shutdown("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_is_alive_no_session() {
        let sm = SessionManager::new("test-repo".to_string());
        assert!(!sm.is_alive("nonexistent").await);
    }

    #[tokio::test]
    async fn test_drain_events_empty() {
        let sm = SessionManager::new("test-repo".to_string());
        let (events, stopped, stderr_by_name) = sm.drain_events().await;
        assert!(events.is_empty());
        assert!(stopped.is_empty());
        assert!(stderr_by_name.is_empty());
    }

    #[tokio::test]
    async fn test_list_names_empty() {
        let sm = SessionManager::new("test-repo".to_string());
        let names = sm.list_names().await;
        assert!(names.is_empty());
    }

    #[tokio::test]
    async fn test_collect_health_empty() {
        let sm = SessionManager::new("test-repo".to_string());
        let health = sm.collect_health().await;
        assert!(health.is_empty());
    }

    #[tokio::test]
    async fn test_list_alive_names_excludes_stopped() {
        let sm = SessionManager::new("test-repo".to_string());

        // Insert a running session and a stopped session
        insert_test_session(&sm, "madison", SessionStatus::Running).await;
        insert_test_session(&sm, "park", SessionStatus::Stopped).await;
        insert_test_session(&sm, "broadway", SessionStatus::Starting).await;

        // list_names returns all sessions (including stopped)
        let all_names = sm.list_names().await;
        assert_eq!(all_names.len(), 3);

        // list_alive_names should exclude the stopped session
        let alive_names = sm.list_alive_names().await;
        assert_eq!(
            alive_names.len(),
            2,
            "list_alive_names should exclude stopped sessions"
        );
        assert!(alive_names.contains(&"madison".to_string()));
        assert!(alive_names.contains(&"broadway".to_string()));
        assert!(
            !alive_names.contains(&"park".to_string()),
            "stopped session 'park' should not be in alive names"
        );
    }

    #[tokio::test]
    async fn test_list_alive_names_empty() {
        let sm = SessionManager::new("test-repo".to_string());
        let names = sm.list_alive_names().await;
        assert!(names.is_empty());
    }
}
