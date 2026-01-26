use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::cli::Response;

/// Client for communicating with the midtown daemon over Unix socket
pub struct DaemonClient {
    socket_path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct Request {
    command: String,
    args: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct DaemonResponse {
    success: bool,
    #[serde(flatten)]
    data: serde_json::Value,
    error: Option<String>,
}

impl DaemonClient {
    /// Connect to the daemon, returning a client handle
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

    /// Get the default socket path
    fn socket_path() -> PathBuf {
        // Try XDG_STATE_HOME first, then fall back to ~/.local/state
        let state_dir = std::env::var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".local")
                    .join("state")
            });

        state_dir.join("midtown").join("daemon.sock")
    }

    /// Send a request to the daemon and get a response
    fn send(&self, command: &str, args: serde_json::Value) -> Result<Response, String> {
        let mut stream =
            UnixStream::connect(&self.socket_path).map_err(|e| format!("Connection failed: {}", e))?;

        let request = Request {
            command: command.to_string(),
            args,
        };

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

        let daemon_response: DaemonResponse = serde_json::from_str(&response_line)
            .map_err(|e| format!("Parse error: {} (response: {})", e, response_line.trim()))?;

        if !daemon_response.success {
            return Err(daemon_response
                .error
                .unwrap_or_else(|| "Unknown error".to_string()));
        }

        // Parse the data into a Response
        serde_json::from_value(daemon_response.data)
            .map_err(|e| format!("Response parse error: {}", e))
    }

    // Channel commands

    pub fn channel_post(&self, message: &str) -> Result<Response, String> {
        self.send(
            "channel.post",
            serde_json::json!({ "message": message }),
        )
    }

    pub fn channel_read(&self, all: bool) -> Result<Response, String> {
        self.send("channel.read", serde_json::json!({ "all": all }))
    }

    // Coworker commands

    pub fn coworker_spawn(&self) -> Result<Response, String> {
        self.send("coworker.spawn", serde_json::json!({}))
    }

    pub fn coworker_shutdown(&self, name: &str) -> Result<Response, String> {
        self.send("coworker.shutdown", serde_json::json!({ "name": name }))
    }

    pub fn coworker_list(&self) -> Result<Response, String> {
        self.send("coworker.list", serde_json::json!({}))
    }

    // Task commands

    pub fn task_create(&self, subject: &str, description: &str) -> Result<Response, String> {
        self.send(
            "task.create",
            serde_json::json!({
                "subject": subject,
                "description": description
            }),
        )
    }

    pub fn task_claim(&self, id: &str) -> Result<Response, String> {
        self.send("task.claim", serde_json::json!({ "id": id }))
    }

    pub fn task_done(&self, id: &str) -> Result<Response, String> {
        self.send("task.done", serde_json::json!({ "id": id }))
    }

    // Status command

    pub fn status(&self) -> Result<Response, String> {
        self.send("status", serde_json::json!({}))
    }

    // PR commands

    pub fn pr_list(&self) -> Result<Response, String> {
        self.send("pr.list", serde_json::json!({}))
    }
}
