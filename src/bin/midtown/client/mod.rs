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

    /// Send a JSON-RPC request to the daemon and get a response.
    fn send(&self, method: &str, params: Option<Value>) -> Result<Response, String> {
        let mut stream = UnixStream::connect(&self.socket_path)
            .map_err(|e| format!("Connection failed: {}", e))?;

        let request = JsonRpcRequest::new(method, params);

        let request_json =
            serde_json::to_string(&request).map_err(|e| format!("Serialization error: {}", e))?;

        // Send request with newline delimiter
        writeln!(stream, "{}", request_json).map_err(|e| format!("Write error: {}", e))?;
        stream.flush().map_err(|e| format!("Flush error: {}", e))?;

        // Read response
        let mut reader = BufReader::new(stream);
        let mut response_line = String::new();
        reader
            .read_line(&mut response_line)
            .map_err(|e| format!("Read error: {}", e))?;

        let rpc_response: JsonRpcResponse = serde_json::from_str(&response_line)
            .map_err(|e| format!("Parse error: {} (response: {})", e, response_line.trim()))?;

        // Check for RPC-level error
        if let Some(error) = rpc_response.error {
            return Err(error.message);
        }

        // Extract result
        let result = rpc_response.result.ok_or("No result in response")?;

        // Parse the result into a Response
        serde_json::from_value(result).map_err(|e| format!("Response parse error: {}", e))
    }

    // Channel commands

    pub fn channel_post(&self, message: &str) -> Result<Response, String> {
        // Use MIDTOWN_AGENT env var for sender, defaulting to "lead"
        let from = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| "lead".to_string());
        self.send(
            "channel.post",
            Some(serde_json::json!({ "message": message, "from": from })),
        )
    }

    pub fn channel_read(&self, all: bool) -> Result<Response, String> {
        self.send("channel.read", Some(serde_json::json!({ "all": all })))
    }

    // Coworker commands

    pub fn coworker_spawn(&self) -> Result<Response, String> {
        self.send("coworker.spawn", None)
    }

    pub fn coworker_shutdown(&self, name: &str) -> Result<Response, String> {
        self.send(
            "coworker.shutdown",
            Some(serde_json::json!({ "name": name })),
        )
    }

    pub fn coworker_list(&self) -> Result<Response, String> {
        self.send("coworker.list", None)
    }

    pub fn coworker_nudge(&self, name: &str, message: Option<&str>) -> Result<Response, String> {
        let mut args = serde_json::json!({ "name": name });
        if let Some(msg) = message {
            args["message"] = serde_json::json!(msg);
        }
        self.send("coworker.nudge", Some(args))
    }

    pub fn coworker_asking(&self, name: &str, question: &str) -> Result<Response, String> {
        self.send(
            "coworker.asking",
            Some(serde_json::json!({ "name": name, "question": question })),
        )
    }

    // Nudge configuration commands

    pub fn nudge_config_show(&self) -> Result<Response, String> {
        self.send("nudge.config.show", Some(serde_json::json!({})))
    }

    pub fn nudge_config_interval(&self, seconds: u64) -> Result<Response, String> {
        self.send(
            "nudge.config.interval",
            Some(serde_json::json!({ "seconds": seconds })),
        )
    }

    pub fn nudge_config_template(&self, template: &str) -> Result<Response, String> {
        self.send(
            "nudge.config.template",
            Some(serde_json::json!({ "template": template })),
        )
    }

    pub fn nudge_config_enable(&self, enabled: bool) -> Result<Response, String> {
        self.send(
            "nudge.config.enable",
            Some(serde_json::json!({ "enabled": enabled })),
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

    pub fn task_claim(&self, id: &str) -> Result<Response, String> {
        self.send("task.claim", Some(serde_json::json!({ "id": id })))
    }

    pub fn task_done(&self, id: &str) -> Result<Response, String> {
        self.send("task.done", Some(serde_json::json!({ "id": id })))
    }

    // Status command

    pub fn status(&self) -> Result<Response, String> {
        self.send("status", None)
    }

    // PR commands

    pub fn pr_list(&self) -> Result<Response, String> {
        self.send("pr.list", None)
    }
}
