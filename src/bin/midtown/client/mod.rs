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
}

/// Request ID counter for JSON-RPC correlation.
static REQUEST_ID: AtomicI64 = AtomicI64::new(1);

/// JSON-RPC 2.0 request.
#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
    id: i64,
}

impl JsonRpcRequest {
    fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0",
            method: method.into(),
            params,
            id: REQUEST_ID.fetch_add(1, Ordering::Relaxed),
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
    /// Connect to the daemon, returning a client handle.
    pub fn connect() -> Result<Self, String> {
        let socket_path = Self::socket_path();

        // Verify socket exists
        if !socket_path.exists() {
            return Err(format!(
                "Daemon socket not found at {}",
                socket_path.display()
            ));
        }

        Ok(DaemonClient { socket_path })
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
        serde_json::from_value(result).map_err(|e| format!("Response parse error: {}", e))
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

        // Set timeouts to prevent hooks from blocking Claude Code indefinitely.
        // PostToolUse hooks run synchronously — Claude waits for the hook to finish.
        // Without timeouts, a busy daemon causes the hook (and Claude) to hang.
        //
        // The daemon's RPC handlers use spawn_blocking for gh CLI calls (status,
        // kanban.data) which can take 2-3 seconds due to GitHub API latency and
        // auth switching. Use a 5-second timeout to accommodate these slow methods
        // while still providing reasonable CLI feedback.
        let timeout = Some(std::time::Duration::from_secs(5));
        stream
            .set_write_timeout(timeout)
            .map_err(|e| format!("Failed to set write timeout: {}", e))?;
        stream
            .set_read_timeout(timeout)
            .map_err(|e| format!("Failed to set read timeout: {}", e))?;

        let request = JsonRpcRequest::new(method, params);

        let request_json =
            serde_json::to_string(&request).map_err(|e| format!("Serialization error: {}", e))?;

        // Send request with newline delimiter
        writeln!(stream, "{}", request_json).map_err(|e| format!("Write error: {}", e))?;
        stream.flush().map_err(|e| format!("Flush error: {}", e))?;

        // Read response with retry logic for EAGAIN/EWOULDBLOCK.
        // These errors occur when the socket has no data yet but isn't closed.
        // We retry up to the socket timeout (5 seconds) to handle slow daemon responses.
        // Note: On macOS, a socket timeout may return WouldBlock instead of TimedOut.
        let mut reader = BufReader::new(stream);
        let mut response_line = String::new();
        let start = std::time::Instant::now();
        let max_duration = std::time::Duration::from_secs(5);

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

    pub fn channel_post(&self, message: &str) -> Result<Response, String> {
        // Use MIDTOWN_AGENT env var for sender, defaulting to "lead"
        let from = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| "lead".to_string());
        self.channel_post_as(message, &from)
    }

    /// Post a message to the channel with an explicit sender.
    ///
    /// This is used by the TUI to post as "user" so the daemon can nudge the Lead.
    pub fn channel_post_as(&self, message: &str, from: &str) -> Result<Response, String> {
        self.send(
            "channel.post",
            Some(serde_json::json!({ "message": message, "from": from })),
        )
    }

    pub fn channel_read(&self, all: bool) -> Result<Response, String> {
        self.send("channel.read", Some(serde_json::json!({ "all": all })))
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

    pub fn coworker_spawn(&self, resume: bool, prompt: Option<&str>) -> Result<Response, String> {
        let mut params = serde_json::json!({ "resume": resume });
        if let Some(p) = prompt {
            params["prompt"] = serde_json::json!(p);
        }
        self.send("coworker.spawn", Some(params))
    }

    pub fn coworker_break(&self, name: &str) -> Result<Response, String> {
        self.send("coworker.break", Some(serde_json::json!({ "name": name })))
    }

    pub fn coworker_list(&self) -> Result<Response, String> {
        self.send("coworker.list", None)
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
    ) -> Result<Response, String> {
        let mut params = serde_json::json!({ "name": name, "phase": phase });
        if let Some(id) = task_id {
            params["task_id"] = serde_json::json!(id);
        }
        self.send("coworker.report-state", Some(params))
    }

    pub fn coworker_asking(&self, name: &str, question: &str) -> Result<Response, String> {
        self.send(
            "coworker.asking",
            Some(serde_json::json!({ "name": name, "question": question })),
        )
    }

    // Task commands

    pub fn task_create(&self, subject: &str, description: &str) -> Result<Response, String> {
        self.send(
            "task.create",
            Some(serde_json::json!({
                "subject": subject,
                "description": description
            })),
        )
    }

    pub fn task_update(
        &self,
        id: &str,
        owner: Option<&str>,
        status: Option<&str>,
        description: Option<&str>,
        blocked_by: Option<&[String]>,
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
    /// (session_id, cwd, name) so the CLI can create a tmux window.
    pub fn session_attach(&self, target: &str) -> Result<Value, String> {
        self.send_raw(
            "session.attach",
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

    // Auth commands

    pub fn auth_switch(&self, profile: &str) -> Result<Response, String> {
        self.send(
            "auth.switch",
            Some(serde_json::json!({ "profile": profile })),
        )
    }

    // Kanban commands

    pub fn kanban_data(&self) -> Result<Value, String> {
        self.send_raw("kanban.data", None)
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
