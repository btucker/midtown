//! Session manager for headless coworker processes.
//!
//! `SessionManager` owns running `HeadlessSession` instances and provides the
//! daemon with spawn/nudge/shutdown/health primitives.
//!
//! The manager runs within the daemon's async runtime. Each coworker session is
//! a child process communicating via stdin/stdout JSON streams.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Datelike, TimeZone, Utc};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::daemon::state::HeadlessSessionInfo;
use crate::headless::{HeadlessConfig, HeadlessSession, StreamEvent};

/// Check if an error message indicates an OAuth token expiry.
///
/// OAuth token expiry errors typically contain:
/// - "OAuth token has expired"
/// - "authentication_error" with HTTP 401
/// - "Invalid authentication credentials"
///
/// Returns true if the error is an auth error that requires re-authentication.
pub(super) fn is_auth_error(error_msg: &str) -> bool {
    let lowercase = error_msg.to_lowercase();
    lowercase.contains("oauth") && lowercase.contains("expired")
        || lowercase.contains("authentication_error")
        || lowercase.contains("invalid authentication")
        || (lowercase.contains("401") && lowercase.contains("unauthorized"))
        || lowercase.contains("not logged in")
}

/// Parse usage limit messages to extract reset time.
///
/// Claude Code usage limit messages typically contain text like:
/// "You've hit your limit · resets 10am (America/Chicago)"
///
/// This function attempts to parse the reset time and convert it to UTC.
/// Returns None if parsing fails or no usage limit message is found.
fn parse_usage_limit_reset_time(error_msg: &str) -> Option<DateTime<Utc>> {
    // Check if this is a usage limit error
    if !error_msg.contains("usage limit") && !error_msg.contains("hit your limit") {
        return None;
    }

    // Try to extract time patterns like "10am", "11:30pm", etc.
    // Pattern: "resets <time> (<timezone>)"
    let re = regex::Regex::new(r"resets?\s+(\d{1,2}):?(\d{2})?\s*(am|pm)\s*\(([^)]+)\)").ok()?;

    if let Some(caps) = re.captures(error_msg) {
        let hour: u32 = caps.get(1)?.as_str().parse().ok()?;
        let minute: u32 = caps.get(2).map_or(0, |m| m.as_str().parse().unwrap_or(0));
        let am_pm = caps.get(3)?.as_str();
        let tz_name = caps.get(4)?.as_str();

        // Convert 12-hour to 24-hour format
        let hour_24 = match (hour, am_pm) {
            (12, "am") => 0,
            (h, "am") => h,
            (12, "pm") => 12,
            (h, "pm") => h + 12,
            _ => return None,
        };

        // Parse timezone
        let tz: chrono_tz::Tz = tz_name.parse().ok()?;

        // Get today's date in that timezone and construct the reset time
        let now = Utc::now().with_timezone(&tz);
        let reset_time = tz
            .with_ymd_and_hms(now.year(), now.month(), now.day(), hour_24, minute, 0)
            .single()?;

        // If the reset time is in the past, it must be tomorrow
        let reset_time_utc = reset_time.with_timezone(&Utc);
        if reset_time_utc < Utc::now() {
            let tomorrow = reset_time + chrono::Duration::days(1);
            Some(tomorrow.with_timezone(&Utc))
        } else {
            Some(reset_time_utc)
        }
    } else {
        // Couldn't parse specific time - return a default (15 minutes from now)
        // This maintains the current behavior as a fallback
        Some(Utc::now() + chrono::Duration::minutes(15))
    }
}

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
    /// Daemon-generated UUID, used as the HashMap key.
    pub slot_id: String,
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
    /// When the usage limit will reset (if known).
    pub usage_limit_reset_at: Option<DateTime<Utc>>,
    /// Whether the session has an API error.
    pub has_api_error: bool,
    /// Whether the session has an authentication error (OAuth token expired).
    pub has_auth_error: bool,
    /// Whether the session has a running subagent.
    pub has_running_subagent: bool,
    /// Whether the session has a pending tool execution (tool_use seen, no tool_result yet).
    pub has_pending_tool: bool,
    /// Whether the session hit "Tool names must be unique" (unrecoverable, needs fresh restart).
    pub has_tool_name_conflict: bool,
    /// Whether this session was spawned as a `--resume` (vs fresh).
    /// Used to detect failed resume attempts: if a resume session exits quickly,
    /// the session_id is stale and should be cleared for fresh spawn.
    pub is_resume: bool,
    /// File handle for writing stream events to JSONL log.
    /// Used for debugging and `midtown coworker view`.
    output_log: Option<std::fs::File>,
    /// Path to the output log file.
    output_log_path: PathBuf,
}

impl CoworkerSession {
    fn new(
        slot_id: String,
        name: String,
        session: HeadlessSession,
        repo: &str,
        session_id: Option<String>,
    ) -> Self {
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
            slot_id,
            name,
            status: SessionStatus::Starting,
            started_at: Utc::now(),
            session_id,
            cost_usd: 0.0,
            last_event_at: None,
            has_usage_limit: false,
            usage_limit_reset_at: None,
            has_api_error: false,
            has_auth_error: false,
            has_running_subagent: false,
            has_pending_tool: false,
            has_tool_name_conflict: false,
            is_resume: false,
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
    /// The `slot_id` is a daemon-generated UUID used as the HashMap key.
    /// If `session_id` is provided, it's set immediately on the CoworkerSession
    /// (used during recovery where resumed sessions don't emit init events).
    pub async fn spawn(
        &self,
        name: &str,
        slot_id: &str,
        config: &HeadlessConfig,
        initial_prompt: Option<&str>,
        session_id: Option<String>,
    ) -> Result<(), crate::Error> {
        // No name-uniqueness check needed — slot_id is always unique (UUID).
        // Multiple sessions with the same name are now allowed (keyed by slot_id).

        // Spawn the headless process
        let mut session = HeadlessSession::spawn(config).map_err(|e| crate::Error::Rpc {
            code: -32603,
            message: format!("Failed to spawn headless session for '{}': {}", name, e),
        })?;

        // Start provider-specific handshake eagerly so sessions with no initial
        // prompt still initialize and expose a session_id to the daemon.
        if let Err(e) = session.ensure_ready().await {
            let _ = session.kill().await;
            return Err(crate::Error::Rpc {
                code: -32603,
                message: format!(
                    "Failed to initialize headless session for '{}': {} — killed session",
                    name, e
                ),
            });
        }

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

        let is_resume = config.resume_session_id.is_some();
        let mut sessions = self.sessions.write().await;
        let mut cs = CoworkerSession::new(
            slot_id.to_string(),
            name.to_string(),
            session,
            &self.repo_name,
            session_id.clone(),
        );
        cs.is_resume = is_resume;
        sessions.insert(slot_id.to_string(), cs);

        if let Some(ref sid) = session_id {
            info!(
                "Spawned headless session for '{}' (slot_id={}, session_id={})",
                name, slot_id, sid
            );
        } else {
            info!(
                "Spawned headless session for '{}' (slot_id={})",
                name, slot_id
            );
        }
        Ok(())
    }

    /// Send a message (nudge) to a running coworker session (by name).
    ///
    /// This writes to the session's stdin via the stream-json input protocol.
    /// Finds the first session matching the name.
    pub async fn send_message(&self, name: &str, message: &str) -> Result<(), crate::Error> {
        let mut sessions = self.sessions.write().await;
        let cs = sessions
            .values_mut()
            .find(|cs| cs.name == name)
            .ok_or_else(|| crate::Error::Rpc {
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

    /// Shut down a coworker session (by name, finds first match).
    ///
    /// Kills the child process immediately via SIGKILL. Use `graceful_shutdown`
    /// when the session needs to persist state before dying (e.g., for attach).
    ///
    /// Returns the session ID (if known) for potential resume.
    pub async fn shutdown(&self, name: &str) -> Result<Option<String>, crate::Error> {
        let mut sessions = self.sessions.write().await;
        let slot_id = sessions
            .values()
            .find(|cs| cs.name == name)
            .map(|cs| cs.slot_id.clone())
            .ok_or_else(|| crate::Error::Rpc {
                code: -32602,
                message: format!("No headless session for '{}'", name),
            })?;
        let cs = sessions
            .remove(&slot_id)
            .expect("slot_id found by name must exist in sessions map");

        let session_id = cs.session_id.clone();

        // The HeadlessSession's Drop impl will kill the child process
        drop(cs);

        info!(
            "Shut down headless session '{}' (session_id={:?})",
            name, session_id
        );
        Ok(session_id)
    }

    /// Gracefully shut down a session, giving it time to persist state.
    ///
    /// Sends SIGTERM so Claude Code can save its session, then waits up to
    /// `timeout` for it to exit. Falls back to SIGKILL only as a last resort.
    /// Used by the attach flow to ensure `--resume` works in the interactive pane.
    pub async fn graceful_shutdown(
        &self,
        name: &str,
        timeout: Duration,
    ) -> Result<Option<String>, crate::Error> {
        let mut sessions = self.sessions.write().await;
        let slot_id = sessions
            .values()
            .find(|cs| cs.name == name)
            .map(|cs| cs.slot_id.clone())
            .ok_or_else(|| crate::Error::Rpc {
                code: -32602,
                message: format!("No headless session for '{}'", name),
            })?;
        let mut cs = sessions
            .remove(&slot_id)
            .expect("slot_id found by name must exist in sessions map");

        let session_id = cs.session_id.clone();

        // Send SIGTERM so Claude Code saves session state and exits cleanly.
        // SIGTERM is the standard graceful-shutdown signal; Claude Code handles
        // it by persisting conversation history before exiting.
        if let Some(ref session) = cs.session
            && let Some(pid) = session.pid()
        {
            let _ = std::process::Command::new("kill")
                .arg(pid.to_string())
                .stderr(std::process::Stdio::null())
                .status();
            info!("Sent SIGTERM to session '{}' (pid={})", name, pid);
        }

        // Drop the write lock while waiting for exit, so we don't block other operations.
        drop(sessions);

        // Wait for the process to exit gracefully after SIGTERM.
        if let Some(ref mut session) = cs.session {
            match tokio::time::timeout(timeout, session.wait()).await {
                Ok(Ok(_status)) => {
                    info!(
                        "Gracefully shut down headless session '{}' (session_id={:?})",
                        name, session_id
                    );
                }
                Ok(Err(e)) => {
                    warn!(
                        "Error waiting for session '{}' to exit: {}. Force-killing.",
                        name, e
                    );
                }
                Err(_) => {
                    warn!(
                        "Session '{}' did not exit within {:?} after SIGTERM. Force-killing.",
                        name, timeout
                    );
                }
            }
        }

        // Drop triggers HeadlessSession::Drop which SIGKILL's if still alive.
        drop(cs);

        Ok(session_id)
    }

    /// Shut down all coworker sessions.
    ///
    /// Called during daemon shutdown to prevent orphaned processes.
    /// Returns the number of sessions that were shut down.
    pub async fn shutdown_all(&self) -> usize {
        let mut sessions = self.sessions.write().await;
        let count = sessions.len();
        let slot_ids: Vec<String> = sessions.keys().cloned().collect();
        for slot_id in &slot_ids {
            if let Some(cs) = sessions.remove(slot_id) {
                let session_id = cs.session_id.clone();
                let name = cs.name.clone();
                drop(cs); // Drop triggers process kill
                info!(
                    "Shut down headless session '{}' during daemon shutdown (session_id={:?})",
                    name, session_id
                );
            }
        }
        count
    }

    /// Gracefully shut down all coworker sessions.
    ///
    /// Sends SIGTERM to each session so Claude Code can save state, then waits
    /// up to `timeout` for all to exit. Any that haven't exited by then are
    /// force-killed via SIGKILL (through the Drop impl).
    ///
    /// Session metadata (name, session_id, etc.) is kept in the map with status
    /// Stopped so that `collect_session_info()` can still read it after this call
    /// returns — this is required for session persistence across daemon restarts.
    ///
    /// Returns the number of sessions that were shut down.
    pub async fn graceful_shutdown_all(&self, timeout: Duration) -> usize {
        // Take the session handles out of the map (send SIGTERM while holding lock),
        // but keep the CoworkerSession entries in the map with status Stopped.
        // This preserves session_id and other metadata for collect_session_info().
        let mut handles: Vec<(String, HeadlessSession)> = Vec::new();
        let total_count;
        {
            let mut sessions = self.sessions.write().await;
            total_count = sessions.len();
            if total_count == 0 {
                return 0;
            }
            for cs in sessions.values_mut() {
                if let Some(session) = cs.session.take() {
                    if let Some(pid) = session.pid() {
                        let _ = std::process::Command::new("kill")
                            .arg(pid.to_string())
                            .stderr(std::process::Stdio::null())
                            .status();
                        info!("Sent SIGTERM to session '{}' (pid={})", cs.name, pid);
                    }
                    handles.push((cs.name.clone(), session));
                }
                cs.status = SessionStatus::Stopped;
            }
        }

        // Wait for all processes to exit within the timeout (lock is released above)
        let deadline = tokio::time::Instant::now() + timeout;
        for (name, session) in &mut handles {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(remaining, session.wait()).await {
                Ok(Ok(_)) => {
                    info!(
                        "Gracefully shut down headless session '{}' after SIGTERM",
                        name
                    );
                }
                Ok(Err(e)) => {
                    warn!("Error waiting for session '{}' to exit: {}", name, e);
                }
                Err(_) => {
                    warn!(
                        "Session '{}' did not exit within {:?} after SIGTERM. Force-killing.",
                        name, timeout
                    );
                }
            }
        }

        // Drop handles — SIGKILL fallback via HeadlessSession::Drop for any still alive
        drop(handles);

        info!("Gracefully shut down {} headless session(s)", total_count);
        total_count
    }

    /// Check if a coworker has a running session (by name).
    pub async fn is_alive(&self, name: &str) -> bool {
        let sessions = self.sessions.read().await;
        sessions
            .values()
            .find(|cs| cs.name == name)
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

        for (_slot_id, cs) in sessions.iter_mut() {
            let session = match cs.session.as_mut() {
                Some(s) => s,
                None => continue,
            };

            let name = &cs.name;
            let mut events = Vec::new();

            // Drain stderr first to prevent pipe buffer deadlock.
            // If stderr writes >64KB without draining, the child process blocks.
            // This must happen every tick, not just on exit.
            let stderr_lines = session.drain_stderr().await;
            // Check for unrecoverable "Tool names must be unique" error.
            // This happens when resuming a session with conflicting tool definitions
            // and cannot be resolved by retrying — the session needs a fresh restart.
            if !cs.has_tool_name_conflict {
                for line in &stderr_lines {
                    if line.contains("Tool names must be unique") {
                        warn!(
                            "Session '{}' hit 'Tool names must be unique' error — needs fresh restart",
                            name
                        );
                        cs.has_tool_name_conflict = true;
                        break;
                    }
                }
            }

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
                                result,
                                extra,
                                ..
                            } => {
                                if let Some(cost) = total_cost_usd {
                                    cs.cost_usd = *cost;
                                }
                                if *is_error {
                                    // Check error type in priority order:
                                    // 1. Auth errors (require user intervention)
                                    // 2. Usage limits (have a reset time)
                                    // 3. Generic API errors (transient)
                                    let error_msg = result.as_deref().unwrap_or("");
                                    let extra_str = extra.to_string();
                                    let combined = format!("{} {}", error_msg, extra_str);

                                    if is_auth_error(&combined) {
                                        // OAuth token expired - requires user re-authentication
                                        cs.has_auth_error = true;
                                        warn!(
                                            "Session '{}' hit auth error (OAuth token expired)",
                                            name
                                        );
                                    } else if let Some(reset_time) =
                                        parse_usage_limit_reset_time(&combined)
                                    {
                                        cs.has_usage_limit = true;
                                        cs.usage_limit_reset_at = Some(reset_time);
                                        debug!(
                                            "Session '{}' hit usage limit, resets at {}",
                                            name, reset_time
                                        );
                                    } else {
                                        // Generic API error (not usage limit or auth)
                                        cs.has_api_error = true;
                                    }
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

    /// Get the session ID for a coworker (if known, by name).
    pub async fn get_session_id(&self, name: &str) -> Option<String> {
        let sessions = self.sessions.read().await;
        sessions
            .values()
            .find(|cs| cs.name == name)
            .and_then(|cs| cs.session_id.clone())
    }

    /// Get the OS process ID for a coworker session (by name, for zombie cleanup).
    pub async fn get_pid(&self, name: &str) -> Option<u32> {
        let sessions = self.sessions.read().await;
        sessions
            .values()
            .find(|cs| cs.name == name)
            .and_then(|cs| cs.session.as_ref())
            .and_then(|session| session.pid())
    }

    /// Mark all sessions to be detached (not killed) on drop.
    ///
    /// Called during daemon shutdown to allow sessions to survive restarts.
    /// The daemon will resume these sessions after restart using the persisted
    /// session IDs.
    pub async fn detach_all(&self) {
        let mut sessions = self.sessions.write().await;
        for cs in sessions.values_mut() {
            if let Some(session) = cs.session.as_mut() {
                session.detach_on_drop();
            }
        }
    }

    /// Collect HeadlessSessionInfo for all running sessions to persist before shutdown.
    ///
    /// Returns a HashMap keyed by coworker name, ready to be saved to persistent state.
    /// The caller should supplement with task/PR/purpose info from CoworkerManager and
    /// GitHub state, then save via `persistent_state.save_for_repo()`.
    pub async fn collect_session_info(&self) -> HashMap<String, HeadlessSessionInfo> {
        let sessions = self.sessions.read().await;
        let mut info_map = HashMap::new();

        for (_slot_id, cs) in sessions.iter() {
            if let Some(session_id) = &cs.session_id {
                let pid = cs.session.as_ref().and_then(|s| s.pid());
                let info = HeadlessSessionInfo {
                    session_id: session_id.clone(),
                    last_active: cs.last_event_at.unwrap_or(cs.started_at),
                    purpose: String::new(), // To be filled by caller
                    pid,
                    coworker_type: None, // To be filled by caller
                    task_id: None,       // To be filled by caller
                    pr_number: None,     // To be filled by caller
                    channel: None,       // To be filled by caller
                    working_dir: None,   // To be filled by caller
                    provider: None,      // To be filled by caller
                    profile: None,       // To be filled by caller
                    resume_on_startup: true,
                    initial_prompt: None, // To be filled by caller
                };
                info_map.insert(cs.name.clone(), info);
            }
        }

        info_map
    }

    /// Collect health data for all sessions.
    ///
    /// Returns a map of coworker name → ProcessHealth for the WorldSnapshot.
    pub async fn collect_health(&self) -> HashMap<String, super::snapshot::ProcessHealth> {
        let sessions = self.sessions.read().await;
        let mut health = HashMap::new();

        for (_slot_id, cs) in sessions.iter() {
            health.insert(
                cs.name.clone(),
                super::snapshot::ProcessHealth {
                    is_alive: cs.session.is_some() && cs.status != SessionStatus::Stopped,
                    last_event_at: cs.last_event_at,
                    has_usage_limit: cs.has_usage_limit,
                    usage_limit_reset_at: cs.usage_limit_reset_at,
                    has_api_error: cs.has_api_error,
                    has_auth_error: cs.has_auth_error,
                    has_running_subagent: cs.has_running_subagent,
                    has_pending_tool: cs.has_pending_tool,
                    has_tool_name_conflict: cs.has_tool_name_conflict,
                    exit_code: None,
                },
            );
        }

        health
    }

    /// List all managed session names (including stopped sessions pending cleanup).
    pub async fn list_names(&self) -> Vec<String> {
        let sessions = self.sessions.read().await;
        sessions.values().map(|cs| cs.name.clone()).collect()
    }

    /// List only alive session names (excludes stopped sessions pending cleanup).
    ///
    /// Use this instead of `list_names()` when building the headless preservation
    /// set for `retain_alive()`. Using `list_names()` includes stopped sessions
    /// that haven't been removed yet, which can cause `retain_alive` to preserve
    /// stale entries in the CoworkerManager tracking map.
    pub async fn list_alive_names(&self) -> Vec<String> {
        let sessions = self.sessions.read().await;
        sessions
            .values()
            .filter(|cs| cs.status != SessionStatus::Stopped)
            .map(|cs| cs.name.clone())
            .collect()
    }

    /// Check all tracked sessions for process liveness using `try_wait()`.
    ///
    /// This is a defense-in-depth backstop for `drain_events()`. If a child
    /// process exits but stdout doesn't close cleanly (pipe buffering, timing
    /// race, etc.), `drain_events` may not detect the exit. This method uses
    /// the kernel's `waitpid(WNOHANG)` to definitively check if each process
    /// is still alive, and force-marks dead sessions as Stopped.
    ///
    /// Returns the names of sessions that were discovered to be dead.
    pub async fn reconcile_process_health(&self) -> Vec<String> {
        let mut sessions = self.sessions.write().await;
        let mut newly_stopped = Vec::new();

        for (_slot_id, cs) in sessions.iter_mut() {
            // Only check sessions that we think are alive
            if cs.status == SessionStatus::Stopped {
                continue;
            }

            let name = &cs.name;
            let session = match cs.session.as_mut() {
                Some(s) => s,
                None => {
                    // No session handle but status isn't Stopped — fix inconsistency
                    warn!(
                        "Session '{}' has no handle but status={:?} — marking as stopped",
                        name, cs.status
                    );
                    cs.status = SessionStatus::Stopped;
                    newly_stopped.push(name.clone());
                    continue;
                }
            };

            match session.try_wait() {
                Ok(Some(exit_status)) => {
                    // Process has exited but drain_events didn't catch it
                    warn!(
                        "Session '{}' process exited (status={}) but was still tracked as {:?} — forcing cleanup",
                        name, exit_status, cs.status
                    );
                    cs.status = SessionStatus::Stopped;
                    cs.session = None;
                    newly_stopped.push(name.clone());
                }
                Ok(None) => {
                    // Process is still running — all good
                }
                Err(e) => {
                    // Error checking process status — treat as dead
                    warn!(
                        "Failed to check process liveness for session '{}': {} — marking as stopped",
                        name, e
                    );
                    cs.status = SessionStatus::Stopped;
                    cs.session = None;
                    newly_stopped.push(name.clone());
                }
            }
        }

        newly_stopped
    }

    /// Check if a session was a failed resume attempt.
    ///
    /// Returns `true` if the session was spawned with `--resume` and exited
    /// within 30 seconds of spawn — meaning the resume itself failed (stale
    /// session_id, no conversation on disk). Sessions that ran longer are
    /// assumed to have valid data on disk that could be resumed next time.
    pub async fn was_failed_resume(&self, name: &str) -> bool {
        let sessions = self.sessions.read().await;
        let cs = match sessions.values().find(|cs| cs.name == name) {
            Some(cs) => cs,
            None => return false,
        };
        if !cs.is_resume {
            return false;
        }
        let age = Utc::now() - cs.started_at;
        age < chrono::Duration::seconds(30)
    }

    /// Remove a stopped session entry (cleanup after the coworker is fully shut down, by name).
    pub async fn remove(&self, name: &str) {
        let log_path = {
            let mut sessions = self.sessions.write().await;
            let slot_id = sessions
                .values()
                .find(|cs| cs.name == name)
                .map(|cs| cs.slot_id.clone());
            let log_path = slot_id
                .as_ref()
                .and_then(|sid| sessions.get(sid).map(|cs| cs.output_log_path.clone()));
            if let Some(ref sid) = slot_id {
                sessions.remove(sid);
            }
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
        // Get the log path: try active sessions first, fall back to the
        // deterministic path for paused/attached/historical sessions.
        let log_path = {
            let sessions = self.sessions.read().await;
            sessions
                .values()
                .find(|cs| cs.name == name)
                .map(|cs| cs.output_log_path.clone())
                .unwrap_or_else(|| crate::paths::headless_output_file(&self.repo_name, name))
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

#[path = "sessions_tests.rs"]
#[cfg(test)]
mod tests;
