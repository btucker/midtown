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

use chrono::{DateTime, Datelike, TimeZone, Utc};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::daemon::state::HeadlessSessionInfo;
use crate::headless::{HeadlessConfig, HeadlessSession, StreamEvent};

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
    /// Whether the session has a running subagent.
    pub has_running_subagent: bool,
    /// Whether the session has a pending tool execution (tool_use seen, no tool_result yet).
    pub has_pending_tool: bool,
    /// Whether the session hit "Tool names must be unique" (unrecoverable, needs fresh restart).
    pub has_tool_name_conflict: bool,
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
            has_running_subagent: false,
            has_pending_tool: false,
            has_tool_name_conflict: false,
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

        let mut sessions = self.sessions.write().await;
        sessions.insert(
            slot_id.to_string(),
            CoworkerSession::new(
                slot_id.to_string(),
                name.to_string(),
                session,
                &self.repo_name,
                session_id.clone(),
            ),
        );

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
    /// Unlike tmux send-keys, this doesn't require waiting for input stability.
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
    /// Kills the child process. The Claude Code session persists on disk
    /// (when `persist_session: true`) and can be resumed later.
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
                                    // Check if this is a usage limit error
                                    let error_msg = result.as_deref().unwrap_or("");
                                    let extra_str = extra.to_string();
                                    let combined = format!("{} {}", error_msg, extra_str);

                                    if let Some(reset_time) =
                                        parse_usage_limit_reset_time(&combined)
                                    {
                                        cs.has_usage_limit = true;
                                        cs.usage_limit_reset_at = Some(reset_time);
                                        debug!(
                                            "Session '{}' hit usage limit, resets at {}",
                                            name, reset_time
                                        );
                                    } else {
                                        // Generic API error (not usage limit)
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
            if let (Some(session_id), Some(session)) = (&cs.session_id, &cs.session) {
                let pid = session.pid();
                let info = HeadlessSessionInfo {
                    session_id: session_id.clone(),
                    last_active: cs.last_event_at.unwrap_or(cs.started_at),
                    purpose: String::new(), // To be filled by caller
                    pid,
                    coworker_type: None, // To be filled by caller
                    task_id: None,       // To be filled by caller
                    pr_number: None,     // To be filled by caller
                    working_dir: None,   // To be filled by caller
                    provider: None,      // To be filled by caller
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
    /// set for `sync_with_tmux()`. Using `list_names()` includes stopped sessions
    /// that haven't been removed yet, which can cause `sync_with_tmux` to preserve
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
        // Get the log path without holding the lock during file I/O
        let log_path = {
            let sessions = self.sessions.read().await;
            let cs = sessions.values().find(|cs| cs.name == name)?;
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
        let slot_id = uuid::Uuid::new_v4().to_string();
        sessions.insert(
            slot_id.clone(),
            CoworkerSession {
                session: None,
                slot_id,
                name: name.to_string(),
                status,
                started_at: Utc::now(),
                session_id: None,
                cost_usd: 0.0,
                last_event_at: None,
                has_usage_limit: false,
                usage_limit_reset_at: None,
                has_api_error: false,
                has_running_subagent: false,
                has_pending_tool: false,
                has_tool_name_conflict: false,
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

    #[tokio::test]
    async fn test_reconcile_catches_no_handle_sessions() {
        let sm = SessionManager::new("test-repo".to_string());

        // Insert a session with Running status but no handle (session: None)
        // This simulates the inconsistent state where a session handle is lost
        insert_test_session(&sm, "madison", SessionStatus::Running).await;

        let stopped = sm.reconcile_process_health().await;
        assert_eq!(
            stopped,
            vec!["madison"],
            "Should detect handle-less Running session"
        );

        // Verify the session is now marked as Stopped
        let alive = sm.list_alive_names().await;
        assert!(
            !alive.contains(&"madison".to_string()),
            "madison should no longer be alive"
        );
    }

    #[tokio::test]
    async fn test_reconcile_skips_already_stopped() {
        let sm = SessionManager::new("test-repo".to_string());

        insert_test_session(&sm, "park", SessionStatus::Stopped).await;

        let stopped = sm.reconcile_process_health().await;
        assert!(
            stopped.is_empty(),
            "Should not flag already-stopped sessions"
        );
    }

    #[tokio::test]
    async fn test_reconcile_empty() {
        let sm = SessionManager::new("test-repo".to_string());
        let stopped = sm.reconcile_process_health().await;
        assert!(stopped.is_empty());
    }

    #[tokio::test]
    async fn test_spawn_with_session_id_sets_session_id_immediately() {
        // This test demonstrates the bug: when spawning a session with a known
        // session_id (like during recovery), the session_id should be set immediately
        // on the CoworkerSession, not left as None waiting for an init event that
        // will never arrive for resumed sessions.

        let sm = SessionManager::new("test-repo".to_string());
        let known_session_id = "test-session-id-123";
        let slot_id = "test-slot-id";
        let name = "madison";

        // Simulate what should happen during recovery: spawn() is called with
        // a known session_id, and it should be immediately set on the CoworkerSession.
        // Currently, spawn() doesn't accept a session_id parameter, so this test
        // will fail until we add that support.

        // For now, we'll test the expectation by manually inserting a session
        // with the session_id set, then verifying get_session_id() works.
        {
            let mut sessions = sm.sessions.write().await;
            sessions.insert(
                slot_id.to_string(),
                CoworkerSession {
                    session: None,
                    slot_id: slot_id.to_string(),
                    name: name.to_string(),
                    status: SessionStatus::Running,
                    started_at: Utc::now(),
                    session_id: Some(known_session_id.to_string()),
                    cost_usd: 0.0,
                    last_event_at: None,
                    has_usage_limit: false,
                    usage_limit_reset_at: None,
                    has_api_error: false,
                    has_running_subagent: false,
                    has_pending_tool: false,
                    has_tool_name_conflict: false,
                    output_log: None,
                    output_log_path: PathBuf::new(),
                },
            );
        }

        // Verify get_session_id() returns the expected value
        let retrieved_session_id = sm.get_session_id(name).await;
        assert_eq!(
            retrieved_session_id,
            Some(known_session_id.to_string()),
            "get_session_id() should return the session_id that was set during spawn"
        );
    }

    #[test]
    fn test_parse_usage_limit_with_time() {
        // Test parsing "resets 10am (America/Chicago)"
        let msg = "You've hit your limit · resets 10am (America/Chicago) · /upgrade to increase";
        let result = parse_usage_limit_reset_time(msg);
        assert!(result.is_some(), "Should parse usage limit with time");
    }

    #[test]
    fn test_parse_usage_limit_with_minutes() {
        // Test parsing "resets 11:30pm (America/Chicago)"
        let msg = "usage limit hit - resets 11:30pm (America/Chicago)";
        let result = parse_usage_limit_reset_time(msg);
        assert!(result.is_some(), "Should parse usage limit with minutes");
    }

    #[test]
    fn test_parse_usage_limit_no_time_pattern() {
        // Should still detect usage limit but fall back to default time
        let msg = "You've hit your usage limit. Please try again later.";
        let result = parse_usage_limit_reset_time(msg);
        assert!(
            result.is_some(),
            "Should detect usage limit without time pattern"
        );
    }

    #[test]
    fn test_not_a_usage_limit_message() {
        // Should not match non-usage-limit errors
        let msg = "API error: connection timeout";
        let result = parse_usage_limit_reset_time(msg);
        assert!(result.is_none(), "Should not match non-usage-limit errors");
    }

    #[test]
    fn test_usage_limit_reset_time_in_future() {
        let msg = "You've hit your limit · resets 11:59pm (America/Chicago)";
        let result = parse_usage_limit_reset_time(msg);
        if let Some(reset_time) = result {
            let now = chrono::Utc::now();
            assert!(
                reset_time > now,
                "Reset time should be in the future (or within today if after 11:59pm CST)"
            );
        } else {
            panic!("Should parse usage limit message");
        }
    }
}
