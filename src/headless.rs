//! Headless Claude Code executor using JSON streaming.
//!
//! Spawns `claude -p --verbose --output-format stream-json` as a child process
//! and communicates via stdin/stdout NDJSON. This provides a lightweight
//! alternative to tmux-based coworkers for structured AI tasks (workflow steps,
//! structured outputs, one-shot queries).
//!
//! ## Protocol
//!
//! **Output** (stdout, NDJSON — one JSON object per line):
//! - `{"type":"system","subtype":"init",...}` — Session metadata
//! - `{"type":"assistant","message":{...}}` — Model response chunks
//! - `{"type":"result","subtype":"success",...}` — Final result with cost/usage
//!
//! **Input** (stdin, when `--input-format stream-json`):
//! - `{"type":"user","message":{"role":"user","content":"..."}}`

use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tracing::{debug, info, warn};

/// Configuration for launching a headless Claude Code session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadlessConfig {
    /// Model to use (e.g., "sonnet", "opus", "haiku").
    pub model: String,
    /// System prompt for the session.
    pub system_prompt: String,
    /// Optional JSON schema for structured output validation.
    pub json_schema: Option<serde_json::Value>,
    /// Working directory for the Claude process. Defaults to current dir.
    pub cwd: Option<String>,
    /// Maximum budget in USD. Defaults to no limit.
    pub max_budget_usd: Option<f64>,
    /// Whether to allow tool use. When false, uses `--tools ""` to disable all tools.
    /// Defaults to false (no tools).
    pub allow_tools: bool,
}

/// Events emitted by a headless Claude Code session.
///
/// These correspond to the NDJSON lines from `--output-format stream-json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum StreamEvent {
    /// Session initialization metadata.
    #[serde(rename = "system")]
    System {
        subtype: String,
        session_id: Option<String>,
        model: Option<String>,
        #[serde(flatten)]
        extra: serde_json::Value,
    },
    /// Assistant message (text or tool_use content blocks).
    #[serde(rename = "assistant")]
    Assistant {
        message: serde_json::Value,
        session_id: Option<String>,
        #[serde(flatten)]
        extra: serde_json::Value,
    },
    /// User message echo (when using stream-json input).
    #[serde(rename = "user")]
    User {
        message: serde_json::Value,
        #[serde(flatten)]
        extra: serde_json::Value,
    },
    /// Final result with cost and usage information.
    #[serde(rename = "result")]
    Result {
        subtype: String,
        is_error: bool,
        result: Option<String>,
        duration_ms: Option<u64>,
        total_cost_usd: Option<f64>,
        session_id: Option<String>,
        #[serde(flatten)]
        extra: serde_json::Value,
    },
}

/// A running headless Claude Code session.
///
/// Owns the child process and provides methods to read streaming events
/// and optionally send follow-up messages.
pub struct HeadlessSession {
    child: Child,
    stdout_reader: BufReader<tokio::process::ChildStdout>,
    stdin: Option<tokio::process::ChildStdin>,
    session_id: Option<String>,
}

impl HeadlessSession {
    /// Spawn a new headless Claude Code session.
    ///
    /// Launches `claude -p --verbose --output-format stream-json` with the
    /// provided configuration. The process is spawned with piped stdin/stdout
    /// for bidirectional JSON streaming.
    pub fn spawn(config: &HeadlessConfig) -> std::io::Result<Self> {
        let mut cmd = Command::new("claude");

        cmd.arg("-p")
            .arg("--verbose")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--input-format")
            .arg("stream-json")
            .arg("--no-session-persistence")
            .arg("--model")
            .arg(&config.model)
            .arg("--system-prompt")
            .arg(&config.system_prompt);

        if let Some(ref schema) = config.json_schema {
            let schema_str = serde_json::to_string(schema)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
            cmd.arg("--json-schema").arg(schema_str);
        }

        if let Some(budget) = config.max_budget_usd {
            cmd.arg("--max-budget-usd").arg(budget.to_string());
        }

        if !config.allow_tools {
            cmd.arg("--tools").arg("");
        }

        // Skip permissions since the daemon manages trust
        cmd.arg("--dangerously-skip-permissions");

        if let Some(ref cwd) = config.cwd {
            cmd.current_dir(cwd);
        }

        // Prevent Claude from inheriting MIDTOWN_AGENT or CLAUDE_CODE_TASK_LIST_ID
        cmd.env_remove("MIDTOWN_AGENT");
        cmd.env_remove("CLAUDE_CODE_TASK_LIST_ID");
        cmd.env("DISABLE_AUTOUPDATER", "1");

        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::null());

        let mut child = cmd.spawn()?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("failed to capture stdout"))?;
        let stdin = child.stdin.take();
        let stdout_reader = BufReader::new(stdout);

        info!("Spawned headless Claude session (model={})", config.model);

        Ok(Self {
            child,
            stdout_reader,
            stdin,
            session_id: None,
        })
    }

    /// Read the next streaming event from the session.
    ///
    /// Returns `None` when the process exits (stdout closes).
    /// Skips blank lines and unparseable lines in a loop (zero-cost,
    /// no heap allocation per skipped line).
    pub async fn next_event(&mut self) -> Option<StreamEvent> {
        loop {
            let mut line = String::new();
            match self.stdout_reader.read_line(&mut line).await {
                Ok(0) => {
                    debug!("Headless session stdout closed");
                    return None;
                }
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<StreamEvent>(trimmed) {
                        Ok(event) => {
                            // Track session_id from init event
                            if let StreamEvent::System {
                                ref subtype,
                                ref session_id,
                                ..
                            } = event
                                && subtype == "init"
                            {
                                self.session_id = session_id.clone();
                            }
                            return Some(event);
                        }
                        Err(e) => {
                            warn!("Failed to parse headless event: {} (line: {})", e, trimmed);
                            continue;
                        }
                    }
                }
                Err(e) => {
                    warn!("Error reading headless session stdout: {}", e);
                    return None;
                }
            }
        }
    }

    /// Send a user message to the session (for multi-turn conversations).
    ///
    /// Requires `--input-format stream-json` (which is set by default).
    pub async fn send_message(&mut self, content: &str) -> std::io::Result<()> {
        let stdin = self.stdin.as_mut().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "stdin not available")
        })?;

        let msg = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": content
            }
        });

        let mut payload = serde_json::to_string(&msg)?;
        payload.push('\n');
        stdin.write_all(payload.as_bytes()).await?;
        stdin.flush().await?;

        debug!("Sent user message to headless session");
        Ok(())
    }

    /// Get the session ID (available after the init event).
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Wait for the process to exit and return the exit status.
    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }

    /// Kill the child process.
    pub async fn kill(&mut self) -> std::io::Result<()> {
        self.child.kill().await
    }
}

impl Drop for HeadlessSession {
    fn drop(&mut self) {
        // Ensure the child process is killed when the session is dropped.
        // tokio::process::Child does NOT kill on drop (it detaches), so we
        // must explicitly start_kill() to prevent orphaned claude processes.
        let _ = self.child.start_kill();
    }
}

/// Execute a one-shot headless query and return the final result.
///
/// This is a convenience function that:
/// 1. Spawns a headless session
/// 2. Sends the prompt
/// 3. Collects all events until the result
/// 4. Returns the result text and cost
///
/// For multi-turn conversations or streaming, use `HeadlessSession` directly.
pub async fn execute(config: &HeadlessConfig, prompt: &str) -> std::io::Result<HeadlessResult> {
    let mut session = HeadlessSession::spawn(config)?;

    // Send the initial prompt
    session.send_message(prompt).await?;

    let mut result_text = None;
    let mut cost_usd = None;
    let mut duration_ms = None;
    let mut is_error = false;
    let mut session_id = None;

    // Collect events until we get the result
    while let Some(event) = session.next_event().await {
        match event {
            StreamEvent::System { ref subtype, .. } if subtype == "init" => {
                session_id = session.session_id().map(String::from);
                debug!("Headless session initialized: {:?}", session_id);
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
                cost_usd = total_cost_usd;
                duration_ms = dur;
                is_error = err;
                if session_id.is_none() {
                    session_id = sid;
                }
                break;
            }
            _ => {
                // Skip intermediate events (assistant messages, tool use, etc.)
            }
        }
    }

    // Wait for the process to exit cleanly
    let _ = session.wait().await;

    Ok(HeadlessResult {
        result: result_text,
        cost_usd,
        duration_ms,
        is_error,
        session_id,
    })
}

/// Result of a one-shot headless execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadlessResult {
    /// The final result text (None if the session produced no result).
    pub result: Option<String>,
    /// Total API cost in USD.
    pub cost_usd: Option<f64>,
    /// Total duration in milliseconds.
    pub duration_ms: Option<u64>,
    /// Whether the result was an error.
    pub is_error: bool,
    /// Session ID from Claude Code.
    pub session_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_headless_config_serialization() {
        let config = HeadlessConfig {
            model: "haiku".to_string(),
            system_prompt: "You are a test assistant.".to_string(),
            json_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "answer": { "type": "string" }
                },
                "required": ["answer"]
            })),
            cwd: None,
            max_budget_usd: Some(0.10),
            allow_tools: false,
        };

        let json = serde_json::to_string(&config).unwrap();
        let parsed: HeadlessConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.model, "haiku");
        assert!(parsed.json_schema.is_some());
    }

    #[test]
    fn test_stream_event_parsing_init() {
        let json = r#"{"type":"system","subtype":"init","cwd":"/tmp","session_id":"abc-123","model":"haiku","tools":[],"mcp_servers":[]}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        match event {
            StreamEvent::System {
                subtype,
                session_id,
                model,
                ..
            } => {
                assert_eq!(subtype, "init");
                assert_eq!(session_id, Some("abc-123".to_string()));
                assert_eq!(model, Some("haiku".to_string()));
            }
            _ => panic!("Expected System event"),
        }
    }

    #[test]
    fn test_stream_event_parsing_result() {
        let json = r#"{"type":"result","subtype":"success","is_error":false,"result":"Hello!","duration_ms":1234,"total_cost_usd":0.001,"session_id":"abc-123"}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        match event {
            StreamEvent::Result {
                subtype,
                is_error,
                result,
                duration_ms,
                total_cost_usd,
                ..
            } => {
                assert_eq!(subtype, "success");
                assert!(!is_error);
                assert_eq!(result, Some("Hello!".to_string()));
                assert_eq!(duration_ms, Some(1234));
                assert_eq!(total_cost_usd, Some(0.001));
            }
            _ => panic!("Expected Result event"),
        }
    }

    #[test]
    fn test_stream_event_parsing_assistant() {
        let json = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hi"}]},"session_id":"abc"}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        match event {
            StreamEvent::Assistant { session_id, .. } => {
                assert_eq!(session_id, Some("abc".to_string()));
            }
            _ => panic!("Expected Assistant event"),
        }
    }

    #[test]
    fn test_headless_result_serialization() {
        let result = HeadlessResult {
            result: Some("42".to_string()),
            cost_usd: Some(0.005),
            duration_ms: Some(2000),
            is_error: false,
            session_id: Some("test-session".to_string()),
        };

        let json = serde_json::to_string(&result).unwrap();
        let parsed: HeadlessResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.result, Some("42".to_string()));
        assert!(!parsed.is_error);
    }
}
