use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cli::Response;

/// Client for communicating with the midtown daemon over Unix socket using JSON-RPC 2.0.
pub struct DaemonClient {
    socket_path: PathBuf,
    /// Timeout for socket read/write operations.
    /// Hooks use short timeouts (5s) since they block Claude Code.
    /// CLI commands use longer timeouts (15s) to tolerate slow GitHub API calls.
    timeout: std::time::Duration,
}

/// Request ID counter for JSON-RPC correlation.
static REQUEST_ID: AtomicI64 = AtomicI64::new(1);

/// Process ID, cached at startup for request ID generation.
static PID: std::sync::LazyLock<u32> = std::sync::LazyLock::new(std::process::id);

/// JSON-RPC 2.0 request.
#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
    id: String,
}

impl JsonRpcRequest {
    fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        let counter = REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        Self {
            jsonrpc: "2.0",
            method: method.into(),
            params,
            id: format!("{}-{}", *PID, counter),
        }
    }
}

/// JSON-RPC 2.0 response.
#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    result: Option<Value>,
    error: Option<JsonRpcError>,
    #[allow(dead_code)]
    id: Value,
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Deserialize)]
struct JsonRpcError {
    #[allow(dead_code)]
    code: i32,
    message: String,
    #[allow(dead_code)]
    data: Option<Value>,
}

impl DaemonClient {
    /// Connect to the daemon with default timeout (15s for CLI commands).
    ///
    /// CLI commands use a 15-second timeout to accommodate slow GitHub API calls
    /// under rate limiting. The daemon now caches PR data, but task/channel operations
    /// may still involve file I/O that can be delayed by spawn_blocking pool contention.
    pub fn connect() -> Result<Self, String> {
        Self::connect_with_timeout(std::time::Duration::from_secs(15))
    }

    /// Connect to the daemon with a short timeout (5s for hooks).
    ///
    /// Hooks run synchronously during Claude Code execution and must not block
    /// for too long. Use this for PostToolUse and other hook contexts.
    pub fn connect_for_hook() -> Result<Self, String> {
        Self::connect_with_timeout(std::time::Duration::from_secs(5))
    }

    /// Connect to the daemon with a custom timeout.
    fn connect_with_timeout(timeout: std::time::Duration) -> Result<Self, String> {
        let socket_path = Self::socket_path();

        // Verify socket exists
        if !socket_path.exists() {
            return Err(format!(
                "Daemon socket not found at {}",
                socket_path.display()
            ));
        }

        Ok(DaemonClient {
            socket_path,
            timeout,
        })
    }

    /// Get the socket path for the current repository.
    ///
    /// Uses git-aware repo detection to ensure clients connect to the
    /// correct daemon for their project.
    fn socket_path() -> PathBuf {
        midtown::paths::daemon_socket()
    }

    /// Send a JSON-RPC request to the daemon and get a typed response.
    fn send(&self, method: &str, params: Option<Value>) -> Result<Response, String> {
        let result = self.send_raw(method, params)?;
        parse_daemon_response(result)
    }

    /// Send a JSON-RPC request and get the raw JSON result value.
    ///
    /// Uses timeouts on the Unix socket to prevent indefinite blocking when the
    /// daemon is busy. This is critical for hook processes — without timeouts,
    /// a slow daemon response stalls the Claude Code instance that fired the hook.
    ///
    /// Includes retry logic for EAGAIN/EWOULDBLOCK errors (os error 35 on macOS,
    /// 11 on Linux) which can occur transiently when the socket buffer is temporarily
    /// unavailable.
    fn send_raw(&self, method: &str, params: Option<Value>) -> Result<Value, String> {
        let mut stream = UnixStream::connect(&self.socket_path)
            .map_err(|e| format!("Connection failed: {}", e))?;

        // Set timeouts to prevent indefinite blocking.
        // Hooks use short timeouts (5s) since they block Claude Code synchronously.
        // CLI commands use longer timeouts (15s) to tolerate slow operations under
        // GitHub API rate limiting or spawn_blocking pool contention.
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(|e| format!("Failed to set write timeout: {}", e))?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(|e| format!("Failed to set read timeout: {}", e))?;

        let request = JsonRpcRequest::new(method, params);

        let request_json =
            serde_json::to_string(&request).map_err(|e| format!("Serialization error: {}", e))?;

        // Send request with newline delimiter
        writeln!(stream, "{}", request_json).map_err(|e| format!("Write error: {}", e))?;
        stream.flush().map_err(|e| format!("Flush error: {}", e))?;

        // Read response with retry logic for EAGAIN/EWOULDBLOCK.
        // These errors occur when the socket has no data yet but isn't closed.
        // We retry up to the socket timeout to handle slow daemon responses.
        // Note: On macOS, a socket timeout may return WouldBlock instead of TimedOut.
        let mut reader = BufReader::new(stream);
        let mut response_line = String::new();
        let start = std::time::Instant::now();
        let max_duration = self.timeout;

        loop {
            match reader.read_line(&mut response_line) {
                Ok(_) => break,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // EAGAIN/EWOULDBLOCK - socket temporarily unavailable or timed out
                    if start.elapsed() >= max_duration {
                        return Err("Read timeout".to_string());
                    }
                    // Brief sleep before retry to avoid busy-spinning
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    continue;
                }
                Err(e) => return Err(format!("Read error: {}", e)),
            }
        }

        let rpc_response: JsonRpcResponse = serde_json::from_str(&response_line)
            .map_err(|e| format!("Parse error: {} (response: {})", e, response_line.trim()))?;

        // Check for RPC-level error
        if let Some(error) = rpc_response.error {
            return Err(error.message);
        }

        // Extract result
        rpc_response
            .result
            .ok_or("No result in response".to_string())
    }

    // Channel commands

    pub fn channel_post(&self, message: &str, channel: Option<&str>) -> Result<Response, String> {
        // Use MIDTOWN_AGENT env var for sender, defaulting to "lead"
        let from = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| "lead".to_string());
        // If no explicit channel provided, check MIDTOWN_CHANNEL env var
        let default_channel = std::env::var("MIDTOWN_CHANNEL").ok();
        let channel = channel.or(default_channel.as_deref());
        self.channel_post_as(message, &from, channel, None)
    }

    /// Post a message as a thread reply.
    ///
    /// Like `channel_post`, but attaches a `thread_parent_id` so the message
    /// is stored as a reply in the specified thread.
    pub fn channel_post_in_thread(
        &self,
        message: &str,
        channel: Option<&str>,
        thread_parent_id: &str,
    ) -> Result<Response, String> {
        let from = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| "lead".to_string());
        let default_channel = std::env::var("MIDTOWN_CHANNEL").ok();
        let channel = channel.or(default_channel.as_deref());
        self.channel_post_as(message, &from, channel, Some(thread_parent_id))
    }

    /// Post a message to the channel with an explicit sender.
    ///
    /// This is used by the TUI to post as "user" so the daemon can nudge the Lead.
    pub fn channel_post_as(
        &self,
        message: &str,
        from: &str,
        channel: Option<&str>,
        thread_parent_id: Option<&str>,
    ) -> Result<Response, String> {
        let mut params = serde_json::json!({ "message": message, "from": from });
        if let Some(ch) = channel {
            params["channel"] = serde_json::Value::String(ch.to_string());
        }
        if let Some(tpi) = thread_parent_id {
            params["thread_parent_id"] = serde_json::Value::String(tpi.to_string());
        }
        self.send("channel.post", Some(params))
    }

    pub fn channel_read(
        &self,
        all: bool,
        last: Option<&usize>,
        since: Option<&str>,
        channel: Option<&str>,
    ) -> Result<Response, String> {
        let mut params = serde_json::json!({ "all": all });
        if let Some(n) = last {
            params["last"] = serde_json::json!(n);
        }
        if let Some(duration) = since {
            params["since"] = serde_json::json!(duration);
        }
        // Explicit --channel flag takes priority over MIDTOWN_CHANNEL env var.
        let resolved_channel = channel
            .map(|s| s.to_string())
            .or_else(|| std::env::var("MIDTOWN_CHANNEL").ok());
        if let Some(ch) = resolved_channel {
            params["channel"] = serde_json::json!(ch);
        }
        self.send("channel.read", Some(params))
    }

    pub fn channel_create(&self, name: &str) -> Result<Response, String> {
        self.send("channel.create", Some(serde_json::json!({ "name": name })))
    }

    pub fn channel_archive(&self, name: &str) -> Result<Response, String> {
        self.send("channel.archive", Some(serde_json::json!({ "name": name })))
    }

    /// List all available channels from the daemon.
    ///
    /// This fetches channels from the daemon's HTTP API (same as web UI),
    /// ensuring TUI and web UI show the same channel list.
    pub fn channel_list(&self, include_archived: bool) -> Result<Response, String> {
        self.send(
            "channel.list",
            Some(serde_json::json!({ "include_archived": include_archived })),
        )
    }

    // Reminder commands

    pub fn reminder_create(&self, trigger: &str, message: &str) -> Result<Response, String> {
        self.send(
            "reminder.create",
            Some(serde_json::json!({ "trigger": trigger, "message": message })),
        )
    }

    pub fn reminder_list(&self) -> Result<Response, String> {
        self.send("reminder.list", None)
    }

    pub fn reminder_cancel(&self, id: &str) -> Result<Response, String> {
        self.send("reminder.cancel", Some(serde_json::json!({ "id": id })))
    }

    // Coworker commands

    pub fn coworker_spawn(
        &self,
        resume: bool,
        prompt: Option<&str>,
        provider: midtown::auth::AuthProvider,
    ) -> Result<Response, String> {
        let mut params = serde_json::json!({ "resume": resume });
        if let Some(p) = prompt {
            params["prompt"] = serde_json::json!(p);
        }
        params["provider"] = serde_json::json!(provider.as_str());
        self.send("coworker.spawn", Some(params))
    }

    pub fn lead_spawn(&self, provider: midtown::auth::AuthProvider) -> Result<Response, String> {
        let params = serde_json::json!({ "provider": provider.as_str() });
        self.send("lead.spawn", Some(params))
    }

    pub fn coworker_break(&self, name: &str) -> Result<Response, String> {
        self.send("coworker.break", Some(serde_json::json!({ "name": name })))
    }

    pub fn coworker_list(&self) -> Result<Response, String> {
        self.send("coworker.list", None)
    }

    /// Uses `send_raw()` instead of `send()` because the RPC response format
    /// `{"success": true, "output": "..."}` doesn't match any `Response` enum variant.
    pub fn coworker_view(&self, name: &str) -> Result<Response, String> {
        let result = self.send_raw("coworker.view", Some(serde_json::json!({ "name": name })))?;
        let output = result
            .get("output")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "RPC response missing 'output' field".to_string())?
            .to_string();
        Ok(Response::message(output))
    }

    pub fn coworker_nudge(&self, name: &str, message: Option<&str>) -> Result<Response, String> {
        // Use MIDTOWN_AGENT env var for sender, defaulting to "lead"
        let from = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| "lead".to_string());
        let mut args = serde_json::json!({ "name": name, "from": from });
        if let Some(msg) = message {
            args["message"] = serde_json::json!(msg);
        }
        self.send("coworker.nudge", Some(args))
    }

    pub fn coworker_report_state(
        &self,
        name: &str,
        phase: &str,
        task_id: Option<u32>,
        progress: Option<u8>,
    ) -> Result<Response, String> {
        let mut params = serde_json::json!({ "name": name, "phase": phase });
        if let Some(id) = task_id {
            params["task_id"] = serde_json::json!(id);
        }
        if let Some(p) = progress {
            params["progress"] = serde_json::json!(p);
        }
        self.send("coworker.report-state", Some(params))
    }

    pub fn coworker_asking(&self, name: &str, question: &str) -> Result<Response, String> {
        self.send(
            "coworker.asking",
            Some(serde_json::json!({ "name": name, "question": question })),
        )
    }

    /// Fetch all pending questions from coworkers waiting for user input.
    ///
    /// Returns the raw JSON value so the caller can parse `questions` array directly.
    pub fn coworker_questions(&self) -> Result<serde_json::Value, String> {
        self.send_raw("coworker.questions", None)
    }

    // Task commands

    #[allow(clippy::too_many_arguments)]
    pub fn task_create(
        &self,
        subject: &str,
        description: &str,
        blocked_by: Option<&[String]>,
        channel: Option<&str>,
        model: Option<&str>,
        pr: Option<u64>,
        plan: Option<&str>,
        execution_skill: Option<&str>,
    ) -> Result<Response, String> {
        let mut params = serde_json::json!({
            "subject": subject,
            "description": description
        });
        if let Some(bb) = blocked_by {
            params["blocked_by"] = serde_json::json!(bb);
        }
        if let Some(ch) = channel {
            params["channel"] = serde_json::json!(ch);
        }
        if let Some(m) = model {
            params["model"] = serde_json::json!(m);
        }
        if let Some(pr_num) = pr {
            params["pr"] = serde_json::json!(pr_num);
        }
        if let Some(p) = plan {
            params["plan"] = serde_json::json!(p);
        }
        if let Some(skill) = execution_skill {
            params["execution_skill"] = serde_json::json!(skill);
        }
        self.send("task.create", Some(params))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn task_update(
        &self,
        id: &str,
        owner: Option<&str>,
        status: Option<&str>,
        description: Option<&str>,
        blocked_by: Option<&[String]>,
        channel: Option<&str>,
        model: Option<&str>,
        pr: Option<u64>,
    ) -> Result<Response, String> {
        let mut params = serde_json::json!({ "id": id });
        if let Some(o) = owner {
            params["owner"] = serde_json::json!(o);
        }
        if let Some(s) = status {
            params["status"] = serde_json::json!(s);
        }
        if let Some(d) = description {
            params["description"] = serde_json::json!(d);
        }
        if let Some(bb) = blocked_by {
            params["blocked_by"] = serde_json::json!(bb);
        }
        if let Some(ch) = channel {
            params["channel"] = serde_json::json!(ch);
        }
        if let Some(m) = model {
            params["model"] = serde_json::json!(m);
        }
        if let Some(pr_num) = pr {
            params["pr"] = serde_json::json!(pr_num);
        }
        self.send("task.update", Some(params))
    }

    pub fn task_claim(&self, id: &str) -> Result<Response, String> {
        let from = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| "unknown".to_string());
        self.send(
            "task.claim",
            Some(serde_json::json!({ "id": id, "from": from })),
        )
    }

    pub fn task_done(&self, id: &str) -> Result<Response, String> {
        self.send("task.done", Some(serde_json::json!({ "id": id })))
    }

    pub fn task_metadata(&self, id: &str) -> Result<serde_json::Value, String> {
        self.send_raw("task.metadata", Some(serde_json::json!({ "id": id })))
    }

    pub fn task_request(&self, description: &str) -> Result<Response, String> {
        // Include the caller's MIDTOWN_AGENT name so the daemon knows who's requesting
        let from = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| "unknown".to_string());
        self.send(
            "task.request",
            Some(serde_json::json!({
                "message": description,
                "from": from
            })),
        )
    }

    // Insight commands

    pub fn report_insight(&self, agent: &str, insight: &str) -> Result<Value, String> {
        self.send_raw(
            "insight.report",
            Some(serde_json::json!({ "agent": agent, "insight": insight })),
        )
    }

    // Session commands (attach/detach headless coworkers)

    /// Request attaching to a headless coworker session.
    ///
    /// The daemon pauses the headless process and returns session info
    /// (session_id, cwd, name) so the CLI can create an interactive pane.
    pub fn session_attach(&self, target: &str) -> Result<Value, String> {
        self.send_raw(
            "session.attach",
            Some(serde_json::json!({ "target": target })),
        )
    }

    /// Resolve a target to one or more attachable sessions.
    pub fn session_resolve(&self, target: &str) -> Result<Value, String> {
        self.send_raw(
            "session.resolve",
            Some(serde_json::json!({ "target": target })),
        )
    }

    /// Notify the daemon that an attached session has been detached.
    ///
    /// The daemon resumes headless execution for the coworker.
    pub fn session_detach(&self, name: &str) -> Result<Response, String> {
        self.send("session.detach", Some(serde_json::json!({ "name": name })))
    }

    /// List all attachable headless sessions.
    pub fn session_list(&self) -> Result<Response, String> {
        self.send("session.list", None)
    }

    /// Clear a session: stop it and relaunch as fresh with the same initial prompt.
    pub fn session_clear(&self, target: &str) -> Result<Response, String> {
        self.send(
            "session.clear",
            Some(serde_json::json!({ "target": target })),
        )
    }

    /// View a session's current output (PTY for headed, JSONL for headless).
    pub fn session_view(&self, target: &str) -> Result<Response, String> {
        let result = self.send_raw(
            "session.view",
            Some(serde_json::json!({ "target": target })),
        )?;
        let output = result
            .get("output")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Ok(Response::message(output))
    }

    // Status command

    pub fn status(&self) -> Result<Response, String> {
        self.send("status", None)
    }

    // PR commands

    pub fn pr_list(&self) -> Result<Response, String> {
        self.send("pr.list", None)
    }

    // Daemon commands

    pub fn check_pending(&self) -> Result<Response, String> {
        self.send("daemon.check-pending", None)
    }

    /// Request the daemon to re-exec itself after graceful shutdown.
    ///
    /// This preserves the daemon's original process context (including sandbox
    /// state), avoiding sandbox-exec nesting failures on macOS.
    pub fn exec_restart(&self) -> Result<Response, String> {
        self.send("daemon.exec-restart", None)
    }

    /// Send SIGTERM to all running headless coworker sessions and wait for them to exit.
    pub fn stop_all_coworkers(&self) -> Result<Response, String> {
        self.send("coworker.stop_all", None)
    }

    // Auth commands

    pub fn auth_switch(
        &self,
        profile: &str,
        all: bool,
        provider: midtown::auth::AuthProvider,
    ) -> Result<Response, String> {
        self.send(
            "auth.switch",
            Some(serde_json::json!({
                "profile": profile,
                "all": all,
                "provider": provider.as_str()
            })),
        )
    }

    // Kanban commands

    #[allow(dead_code)] // Kept for backward compatibility with kanban.data RPC
    pub fn kanban_data(&self) -> Result<Value, String> {
        self.send_raw("kanban.data", None)
    }

    /// Fetch live coworker state via `coworkers.status` RPC.
    ///
    /// Returns coworker phase, health, task assignments, lead activity, and
    /// tool call activity. No GraphQL — intended for fast 1-2s polling.
    pub fn coworkers_status(&self) -> Result<Value, String> {
        self.send_raw("coworkers.status", None)
    }

    /// Fetch PR data via `prs.status` RPC.
    ///
    /// Returns open and recently merged PRs with CI status. Backed by a 60s
    /// server-side cache — intended for slower 30s polling.
    pub fn prs_status(&self) -> Result<Value, String> {
        self.send_raw("prs.status", None)
    }

    // Headless execution

    /// Execute a headless Claude Code session via the daemon.
    ///
    /// Uses a longer timeout (120s) since headless execution waits for the
    /// Claude API response, which can take significant time.
    pub fn headless_execute(
        &self,
        prompt: &str,
        model: &str,
        system_prompt: &str,
        json_schema: Option<Value>,
        max_budget_usd: Option<f64>,
        allow_tools: bool,
    ) -> Result<Value, String> {
        let mut params = serde_json::json!({
            "prompt": prompt,
            "model": model,
            "system_prompt": system_prompt,
            "allow_tools": allow_tools,
        });
        if let Some(schema) = json_schema {
            params["json_schema"] = schema;
        }
        if let Some(budget) = max_budget_usd {
            params["max_budget_usd"] = serde_json::json!(budget);
        }
        self.send_raw_with_timeout("headless.execute", Some(params), 120)
    }

    // Headed wrapper intercom commands

    pub fn headed_register(
        &self,
        session: &str,
        adapter_id: &str,
        provider: midtown::auth::AuthProvider,
    ) -> Result<Value, String> {
        self.send_raw(
            "headed.register",
            Some(serde_json::json!({
                "session": session,
                "adapter_id": adapter_id,
                "provider": provider.as_str()
            })),
        )
    }

    pub fn headed_unregister(&self, session: &str, adapter_id: &str) -> Result<Value, String> {
        self.send_raw(
            "headed.unregister",
            Some(serde_json::json!({
                "session": session,
                "adapter_id": adapter_id
            })),
        )
    }

    pub fn headed_heartbeat(&self, session: &str, adapter_id: &str) -> Result<Value, String> {
        self.send_raw(
            "headed.heartbeat",
            Some(serde_json::json!({
                "session": session,
                "adapter_id": adapter_id
            })),
        )
    }

    pub fn headed_poll(
        &self,
        session: &str,
        adapter_id: &str,
        after_id: u64,
        limit: usize,
    ) -> Result<Value, String> {
        self.send_raw(
            "headed.poll",
            Some(serde_json::json!({
                "session": session,
                "adapter_id": adapter_id,
                "after_id": after_id,
                "limit": limit
            })),
        )
    }

    pub fn headed_ack(
        &self,
        session: &str,
        adapter_id: &str,
        msg_id: u64,
    ) -> Result<Value, String> {
        self.send_raw(
            "headed.ack",
            Some(serde_json::json!({
                "session": session,
                "adapter_id": adapter_id,
                "msg_id": msg_id
            })),
        )
    }

    pub fn headed_output(&self, session: &str, output: &str) -> Result<Value, String> {
        self.send_raw(
            "headed.output",
            Some(serde_json::json!({
                "session": session,
                "output": output
            })),
        )
    }

    /// Enqueue a raw Ctrl+V keystroke (\x16) to a headed session's intercom queue.
    ///
    /// Causes the headed wrapper to write \x16 to the Claude PTY,
    /// triggering Claude's built-in clipboard image paste handler.
    #[allow(dead_code)] // Called by App::send_image_to_lead() in Task 3+
    pub fn headed_enqueue_ctrl_v(&self, session: &str) -> Result<Response, String> {
        self.send(
            "headed.enqueue",
            Some(serde_json::json!({ "session": session, "text": "\x16" })),
        )
    }

    /// Send a JSON-RPC request with a custom timeout in seconds.
    fn send_raw_with_timeout(
        &self,
        method: &str,
        params: Option<Value>,
        timeout_secs: u64,
    ) -> Result<Value, String> {
        let mut stream = UnixStream::connect(&self.socket_path)
            .map_err(|e| format!("Connection failed: {}", e))?;

        let timeout = Some(std::time::Duration::from_secs(timeout_secs));
        stream
            .set_write_timeout(Some(std::time::Duration::from_secs(5)))
            .map_err(|e| format!("Failed to set write timeout: {}", e))?;
        stream
            .set_read_timeout(timeout)
            .map_err(|e| format!("Failed to set read timeout: {}", e))?;

        let request = JsonRpcRequest::new(method, params);
        let request_json =
            serde_json::to_string(&request).map_err(|e| format!("Serialization error: {}", e))?;

        writeln!(stream, "{}", request_json).map_err(|e| format!("Write error: {}", e))?;
        stream.flush().map_err(|e| format!("Flush error: {}", e))?;

        let mut reader = BufReader::new(stream);
        let mut response_line = String::new();
        let start = std::time::Instant::now();
        let max_duration = std::time::Duration::from_secs(timeout_secs);

        loop {
            match reader.read_line(&mut response_line) {
                Ok(_) => break,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if start.elapsed() >= max_duration {
                        return Err("Read timeout".to_string());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    continue;
                }
                Err(e) => return Err(format!("Read error: {}", e)),
            }
        }

        let rpc_response: JsonRpcResponse = serde_json::from_str(&response_line)
            .map_err(|e| format!("Parse error: {} (response: {})", e, response_line.trim()))?;

        if let Some(error) = rpc_response.error {
            return Err(error.message);
        }

        rpc_response
            .result
            .ok_or("No result in response".to_string())
    }
}

/// Normalize daemon RPC result payloads into the CLI response enum.
///
/// This keeps CLI parsing resilient when daemon handlers add envelope fields
/// like `success`, and provides a stable fallback (`Response::Json`) for
/// unexpected payload shapes.
fn parse_daemon_response(value: Value) -> Result<Response, String> {
    if let Ok(parsed) = serde_json::from_value::<Response>(value.clone()) {
        return Ok(parsed);
    }

    // Canonical message fast-path.
    if let Some(message) = value.get("message").and_then(|v| v.as_str()) {
        return Ok(Response::Message {
            message: message.to_string(),
        });
    }

    // Known collection shapes (some handlers include extra envelope fields).
    if let Some(obj) = value.as_object() {
        if let Some(coworkers) = obj.get("coworkers")
            && let Ok(parsed) = serde_json::from_value::<Response>(
                serde_json::json!({ "coworkers": coworkers.clone() }),
            )
        {
            return Ok(parsed);
        }
        if let Some(messages) = obj.get("messages")
            && let Ok(parsed) = serde_json::from_value::<Response>(
                serde_json::json!({ "messages": messages.clone() }),
            )
        {
            return Ok(parsed);
        }
        if let Some(tasks) = obj.get("tasks")
            && let Ok(parsed) =
                serde_json::from_value::<Response>(serde_json::json!({ "tasks": tasks.clone() }))
        {
            return Ok(parsed);
        }
        if let Some(pull_requests) = obj.get("pull_requests")
            && let Ok(parsed) = serde_json::from_value::<Response>(
                serde_json::json!({ "pull_requests": pull_requests.clone() }),
            )
        {
            return Ok(parsed);
        }
        if let Some(sessions) = obj.get("sessions")
            && let Ok(parsed) = serde_json::from_value::<Response>(
                serde_json::json!({ "sessions": sessions.clone() }),
            )
        {
            return Ok(parsed);
        }
    }

    Ok(Response::Json { value })
}

#[path = "channel_read_tests.rs"]
#[cfg(test)]
mod channel_read_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_daemon_response_sessions_payload() {
        let value = serde_json::json!({
            "success": true,
            "sessions": [{
                "name": "park",
                "session_id": "abc-123",
                "status": "running"
            }]
        });

        let parsed = parse_daemon_response(value).expect("parse response");
        match parsed {
            Response::Sessions { sessions } => {
                assert_eq!(sessions.len(), 1);
                assert_eq!(sessions[0].name, "park");
                assert_eq!(sessions[0].session_id, "abc-123");
            }
            other => panic!("Expected Sessions, got {:?}", other),
        }
    }

    #[test]
    fn parse_daemon_response_message_payload() {
        let value = serde_json::json!({
            "type": "message",
            "message": "ok"
        });

        let parsed = parse_daemon_response(value).expect("parse response");
        match parsed {
            Response::Message { message } => assert_eq!(message, "ok"),
            other => panic!("Expected Message, got {:?}", other),
        }
    }

    #[test]
    fn parse_daemon_response_unknown_payload_falls_back_to_json() {
        let value = serde_json::json!({
            "foo": "bar",
            "count": 3
        });

        let parsed = parse_daemon_response(value.clone()).expect("parse response");
        match parsed {
            Response::Json { value: v } => assert_eq!(v, value),
            other => panic!("Expected Json fallback, got {:?}", other),
        }
    }
}
