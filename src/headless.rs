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

use std::collections::VecDeque;
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tracing::{debug, info, warn};

/// Configuration for launching a headless Claude Code session.
///
/// Supports two modes:
/// - **One-shot**: `persist_session: false` (default) — session is not persisted,
///   suitable for structured queries via `execute()`.
/// - **Long-running**: `persist_session: true` — session is persisted on disk,
///   suitable for coworker sessions that may be resumed later.
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
    /// Whether to persist the session on disk. When false, passes
    /// `--no-session-persistence`. Defaults to false (one-shot mode).
    #[serde(default)]
    pub persist_session: bool,
    /// Resume a specific saved session instead of starting fresh.
    /// When set, uses `claude --resume <id>` and omits `--system-prompt`/`--json-schema`.
    #[serde(default)]
    pub resume_session_id: Option<String>,
    /// Auto-break after N seconds of no output from the session.
    /// Monitored externally by the caller (not enforced within HeadlessSession).
    #[serde(default)]
    #[serde(with = "optional_duration_secs")]
    pub inactivity_timeout: Option<Duration>,
    /// Agent teams team name. When set, adds `--team-name` CLI flag and sets
    /// `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` env var.
    #[serde(default)]
    pub team_name: Option<String>,
    /// Agent teams agent ID. When set, adds `--agent-id` CLI flag.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// Agent teams agent name. When set, adds `--agent-name` CLI flag.
    #[serde(default)]
    pub agent_name: Option<String>,
    /// Path to a Claude Code settings JSON file. When set, adds `--settings` flag.
    #[serde(default)]
    pub settings_path: Option<String>,
    /// Setting sources to use. When set, adds `--setting-sources` CLI flag.
    /// Common value: "project,local" to exclude user-level settings.
    #[serde(default)]
    pub setting_sources: Option<String>,
    /// Auth provider backing this session (`claude` or `codex`).
    #[serde(default)]
    pub auth_provider: crate::auth::AuthProvider,
    /// Additional environment variables to set on the child process.
    ///
    /// Applied after the default env_remove call (MIDTOWN_AGENT), so values here
    /// take precedence. Use this to pass coworker-specific env vars like
    /// `MIDTOWN_AGENT` and provider config vars (`CLAUDE_CONFIG_DIR`/`CODEX_HOME`).
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

/// Custom serde module for `Option<Duration>` as seconds (f64).
mod optional_duration_secs {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(value: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(d) => d.as_secs_f64().serialize(serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt: Option<f64> = Option::deserialize(deserializer)?;
        Ok(opt.map(Duration::from_secs_f64))
    }
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

#[derive(Debug, Clone)]
enum SessionProtocol {
    Claude,
    Codex(Box<CodexProtocolState>),
}

#[derive(Debug, Clone)]
struct CodexProtocolState {
    initialized: bool,
    start_request_id: Option<u64>,
    thread_id: Option<String>,
    turn_in_progress: bool,
    next_request_id: u64,
    pending_messages: VecDeque<String>,
    latest_agent_message: Option<String>,
    resume_thread_id: Option<String>,
    model: String,
    cwd: Option<String>,
    system_prompt: String,
    output_schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexPostAction {
    None,
    DispatchPendingTurns,
}

fn codex_translate_event(
    parsed: &serde_json::Value,
    state: &mut CodexProtocolState,
    session_id: &mut Option<String>,
) -> (Option<StreamEvent>, CodexPostAction) {
    // JSON-RPC response: look for thread start/resume completion.
    if parsed.get("id").is_some() {
        let start_request_id = state.start_request_id.unwrap_or(0);
        let response_id = parsed
            .get("id")
            .and_then(|v| v.as_u64())
            .unwrap_or_default();

        if response_id == start_request_id
            && let Some(msg) = parsed
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
        {
            return (
                Some(StreamEvent::Result {
                    subtype: "error".to_string(),
                    is_error: true,
                    result: Some(msg.to_string()),
                    duration_ms: None,
                    total_cost_usd: None,
                    session_id: session_id.clone(),
                    extra: serde_json::json!({ "provider": "codex", "phase": "thread/start" }),
                }),
                CodexPostAction::None,
            );
        }

        if response_id == start_request_id
            && let Some(thread_id) = parsed
                .get("result")
                .and_then(|r| r.get("thread"))
                .and_then(|t| t.get("id"))
                .and_then(|id| id.as_str())
                .map(str::to_string)
        {
            *session_id = Some(thread_id.clone());
            state.thread_id = Some(thread_id.clone());
            return (
                Some(StreamEvent::System {
                    subtype: "init".to_string(),
                    session_id: Some(thread_id),
                    model: Some(state.model.clone()),
                    extra: serde_json::json!({ "provider": "codex" }),
                }),
                CodexPostAction::DispatchPendingTurns,
            );
        }
        return (None, CodexPostAction::None);
    }

    let Some(method) = parsed.get("method").and_then(|v| v.as_str()) else {
        return (None, CodexPostAction::None);
    };
    let params = parsed
        .get("params")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    match method {
        "thread/started" => {
            if session_id.is_none()
                && let Some(thread_id) = params
                    .get("thread")
                    .and_then(|t| t.get("id"))
                    .and_then(|id| id.as_str())
                    .map(str::to_string)
            {
                *session_id = Some(thread_id.clone());
                state.thread_id = Some(thread_id.clone());
                return (
                    Some(StreamEvent::System {
                        subtype: "init".to_string(),
                        session_id: Some(thread_id),
                        model: Some(state.model.clone()),
                        extra: serde_json::json!({ "provider": "codex" }),
                    }),
                    CodexPostAction::DispatchPendingTurns,
                );
            }
        }
        "item/agentMessage/delta" => {
            if let Some(delta) = params.get("delta").and_then(|v| v.as_str()) {
                let next = state.latest_agent_message.take().unwrap_or_default() + delta;
                state.latest_agent_message = Some(next);
                return (
                    Some(StreamEvent::Assistant {
                        message: serde_json::json!({
                            "role": "assistant",
                            "content": [{ "type": "text", "text": delta }]
                        }),
                        session_id: session_id.clone(),
                        extra: serde_json::json!({ "provider": "codex" }),
                    }),
                    CodexPostAction::None,
                );
            }
        }
        "item/completed" => {
            if let Some(text) = params
                .get("item")
                .and_then(|i| i.get("type"))
                .and_then(|t| t.as_str())
                .filter(|t| *t == "agentMessage")
                .and_then(|_| params.get("item"))
                .and_then(|i| i.get("text"))
                .and_then(|t| t.as_str())
                .map(str::to_string)
            {
                state.latest_agent_message = Some(text.clone());
                return (
                    Some(StreamEvent::Assistant {
                        message: serde_json::json!({
                            "role": "assistant",
                            "content": [{ "type": "text", "text": text }]
                        }),
                        session_id: session_id.clone(),
                        extra: serde_json::json!({ "provider": "codex", "event": "item/completed" }),
                    }),
                    CodexPostAction::None,
                );
            }
        }
        "turn/completed" => {
            let status = params
                .get("turn")
                .and_then(|t| t.get("status"))
                .and_then(|s| s.as_str())
                .unwrap_or("failed");
            let is_error = status != "completed";
            let result_text = if is_error {
                params
                    .get("turn")
                    .and_then(|t| t.get("error"))
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .map(str::to_string)
            } else {
                state.latest_agent_message.clone()
            };
            state.turn_in_progress = false;
            return (
                Some(StreamEvent::Result {
                    subtype: if is_error {
                        "error".to_string()
                    } else {
                        "success".to_string()
                    },
                    is_error,
                    result: result_text,
                    duration_ms: None,
                    total_cost_usd: None,
                    session_id: session_id.clone(),
                    extra: serde_json::json!({ "provider": "codex", "status": status }),
                }),
                CodexPostAction::DispatchPendingTurns,
            );
        }
        _ => {}
    }

    (None, CodexPostAction::None)
}

/// A running headless Claude Code session.
///
/// Owns the child process and provides methods to read streaming events
/// and optionally send follow-up messages.
pub struct HeadlessSession {
    child: Child,
    stdout_reader: BufReader<tokio::process::ChildStdout>,
    stderr_reader: BufReader<tokio::process::ChildStderr>,
    stdin: Option<tokio::process::ChildStdin>,
    session_id: Option<String>,
    protocol: SessionProtocol,
    /// When true, don't kill the child process on drop (for daemon restart survival).
    detach_on_drop: bool,
}

impl HeadlessSession {
    /// Spawn a new headless Claude Code session.
    ///
    /// Launches the provider CLI with the provided configuration. The process is spawned
    /// with piped stdin/stdout for bidirectional JSON streaming.
    ///
    /// Two modes:
    /// - **Fresh session** (`resume_session_id: None`): Uses `-p --system-prompt ...`
    /// - **Resume session** (`resume_session_id: Some(id)`): Uses `--resume <id>`,
    ///   omits `--system-prompt` and `--json-schema`.
    pub fn spawn(config: &HeadlessConfig) -> std::io::Result<Self> {
        let is_resume = config.resume_session_id.is_some();
        let mut cmd = match config.auth_provider {
            crate::auth::AuthProvider::Claude | crate::auth::AuthProvider::Zai => {
                // Compute sandbox writable dirs from cwd (project working directory).
                let primary_repo = config
                    .cwd
                    .as_deref()
                    .map(std::path::Path::new)
                    .unwrap_or(std::path::Path::new("/tmp"));
                let writable = crate::sandbox::writable_dirs(primary_repo, &[]);

                // On macOS, wrap with sandbox-exec to restrict filesystem writes.
                // On Linux, wrap with bwrap if available.
                // Falls back to running claude directly if sandbox setup fails.
                let mut cmd = if cfg!(target_os = "macos") {
                    match crate::sandbox::sandbox_exec_prefix(&writable) {
                        Ok((_profile_path, prefix)) => {
                            let mut c = Command::new("sandbox-exec");
                            for arg in &prefix {
                                c.arg(arg);
                            }
                            c.arg("claude");
                            c
                        }
                        Err(e) => {
                            warn!("Sandbox setup failed, running without sandbox: {}", e);
                            Command::new("claude")
                        }
                    }
                } else if cfg!(target_os = "linux") && crate::sandbox::bwrap_available() {
                    let mut c = Command::new("bwrap");
                    // Build bwrap args without the program args (we'll add claude args below)
                    c.args(["--ro-bind", "/", "/"]);
                    for dir in &writable {
                        c.args(["--bind", dir, dir]);
                    }
                    c.args(["--dev", "/dev", "--proc", "/proc", "--", "claude"]);
                    c
                } else {
                    Command::new("claude")
                };

                if is_resume {
                    // Resume mode: --resume <id>, no -p flag
                    cmd.arg("--resume")
                        .arg(config.resume_session_id.as_ref().unwrap());
                } else {
                    // Fresh mode: -p with system prompt
                    cmd.arg("-p");
                    cmd.arg("--system-prompt").arg(&config.system_prompt);

                    if let Some(ref schema) = config.json_schema {
                        let schema_str = serde_json::to_string(schema).map_err(|e| {
                            std::io::Error::new(std::io::ErrorKind::InvalidInput, e)
                        })?;
                        cmd.arg("--json-schema").arg(schema_str);
                    }
                }

                cmd.arg("--verbose")
                    .arg("--output-format")
                    .arg("stream-json")
                    .arg("--input-format")
                    .arg("stream-json")
                    .arg("--model")
                    .arg(&config.model);

                // Session persistence: only add --no-session-persistence when explicitly disabled
                if !config.persist_session {
                    cmd.arg("--no-session-persistence");
                }

                if let Some(budget) = config.max_budget_usd {
                    cmd.arg("--max-budget-usd").arg(budget.to_string());
                }

                if !config.allow_tools {
                    cmd.arg("--tools").arg("");
                }

                // Skip permissions since the daemon manages trust
                cmd.arg("--dangerously-skip-permissions");

                // Agent teams flags
                if let Some(ref team) = config.team_name {
                    cmd.arg("--team-name").arg(team);
                }
                if let Some(ref agent_id) = config.agent_id {
                    cmd.arg("--agent-id").arg(agent_id);
                }
                if let Some(ref agent_name) = config.agent_name {
                    cmd.arg("--agent-name").arg(agent_name);
                }

                // Settings file — skip on resume to avoid duplicate tool registrations.
                // Resumed sessions already have their plugins loaded from saved state;
                // passing --settings again causes "Tool names must be unique" API errors.
                if !is_resume {
                    if let Some(ref settings) = config.settings_path {
                        cmd.arg("--settings").arg(settings);
                    }

                    // Setting sources — also skip on resume for consistency
                    if let Some(ref sources) = config.setting_sources {
                        cmd.arg("--setting-sources").arg(sources);
                    }
                }

                cmd
            }
            crate::auth::AuthProvider::Codex => {
                // Codex app-server runs a persistent JSON-RPC stdio server.
                // We initialize and start/resume threads via requests in `ensure_ready()`.
                let primary_repo = config
                    .cwd
                    .as_deref()
                    .map(std::path::Path::new)
                    .unwrap_or(std::path::Path::new("/tmp"));
                let writable = crate::sandbox::writable_dirs(primary_repo, &[]);

                let mut cmd = if cfg!(target_os = "macos") {
                    match crate::sandbox::sandbox_exec_prefix(&writable) {
                        Ok((_profile_path, prefix)) => {
                            let mut c = Command::new("sandbox-exec");
                            for arg in &prefix {
                                c.arg(arg);
                            }
                            c.arg("codex");
                            c
                        }
                        Err(e) => {
                            warn!(
                                "Sandbox setup failed for codex, running without sandbox: {}",
                                e
                            );
                            Command::new("codex")
                        }
                    }
                } else if cfg!(target_os = "linux") && crate::sandbox::bwrap_available() {
                    let mut c = Command::new("bwrap");
                    c.args(["--ro-bind", "/", "/"]);
                    for dir in &writable {
                        c.args(["--bind", dir, dir]);
                    }
                    c.args(["--dev", "/dev", "--proc", "/proc", "--", "codex"]);
                    c
                } else {
                    Command::new("codex")
                };
                cmd.arg("app-server");
                cmd
            }
        };

        if let Some(ref cwd) = config.cwd {
            cmd.current_dir(cwd);
        }

        // Clear inherited daemon env vars, then re-apply from config.env
        // so coworker-specific values (MIDTOWN_AGENT) take effect.
        cmd.env_remove("MIDTOWN_AGENT");
        cmd.env("DISABLE_AUTOUPDATER", "1");

        // Agent teams requires this env var
        if config.team_name.is_some() && config.auth_provider == crate::auth::AuthProvider::Claude {
            cmd.env("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS", "1");
        }

        // Apply caller-provided env vars (e.g., MIDTOWN_AGENT for coworker identity)
        for (key, value) in &config.env {
            cmd.env(key, value);
        }

        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = cmd.spawn()?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("failed to capture stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("failed to capture stderr"))?;
        let stdin = child.stdin.take();
        let stdout_reader = BufReader::new(stdout);
        let stderr_reader = BufReader::new(stderr);

        let protocol = match config.auth_provider {
            crate::auth::AuthProvider::Claude | crate::auth::AuthProvider::Zai => {
                SessionProtocol::Claude
            }
            crate::auth::AuthProvider::Codex => {
                SessionProtocol::Codex(Box::new(CodexProtocolState {
                    initialized: false,
                    start_request_id: None,
                    thread_id: None,
                    turn_in_progress: false,
                    next_request_id: 1,
                    pending_messages: VecDeque::new(),
                    latest_agent_message: None,
                    resume_thread_id: config.resume_session_id.clone(),
                    model: config.model.clone(),
                    cwd: config.cwd.clone(),
                    system_prompt: config.system_prompt.clone(),
                    output_schema: config.json_schema.clone(),
                }))
            }
        };

        info!(
            "Spawned headless {:?} session (model={}, resume={})",
            config.auth_provider, config.model, is_resume
        );

        Ok(Self {
            child,
            stdout_reader,
            stderr_reader,
            stdin,
            session_id: None,
            protocol,
            detach_on_drop: false,
        })
    }

    /// Convenience method to spawn a session that resumes a previous session.
    ///
    /// Creates a config with `resume_session_id` set and `persist_session: true`,
    /// clears `system_prompt` and `json_schema` (not used in resume mode).
    pub fn resume(session_id: &str, base_config: &HeadlessConfig) -> std::io::Result<Self> {
        let config = HeadlessConfig {
            resume_session_id: Some(session_id.to_string()),
            persist_session: true,
            system_prompt: String::new(), // Not used in resume mode
            json_schema: None,            // Not used in resume mode
            ..base_config.clone()
        };
        Self::spawn(&config)
    }

    fn codex_state_mut(&mut self) -> Option<&mut CodexProtocolState> {
        match &mut self.protocol {
            SessionProtocol::Codex(state) => Some(state.as_mut()),
            SessionProtocol::Claude => None,
        }
    }

    async fn write_json_line(&mut self, value: &serde_json::Value) -> std::io::Result<()> {
        let stdin = self.stdin.as_mut().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "stdin not available")
        })?;
        let mut payload = serde_json::to_string(value)?;
        payload.push('\n');
        stdin.write_all(payload.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn codex_send_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> std::io::Result<u64> {
        let request_id = {
            let state = self.codex_state_mut().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a codex session")
            })?;
            let id = state.next_request_id;
            state.next_request_id += 1;
            id
        };
        self.write_json_line(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params
        }))
        .await?;
        Ok(request_id)
    }

    async fn codex_dispatch_pending_turns(&mut self) -> std::io::Result<()> {
        loop {
            let (thread_id, prompt, model, output_schema) = {
                let state = self.codex_state_mut().ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a codex session")
                })?;
                let Some(thread_id) = state.thread_id.clone() else {
                    return Ok(());
                };
                if state.turn_in_progress {
                    return Ok(());
                }
                let Some(prompt) = state.pending_messages.pop_front() else {
                    return Ok(());
                };
                state.turn_in_progress = true;
                state.latest_agent_message = None;
                (
                    thread_id,
                    prompt,
                    state.model.clone(),
                    state.output_schema.clone(),
                )
            };

            let mut params = serde_json::json!({
                "threadId": thread_id,
                "input": [{ "type": "text", "text": prompt }],
            });
            if !model.is_empty() {
                params["model"] = serde_json::json!(model);
            }
            if let Some(schema) = output_schema {
                params["outputSchema"] = schema;
            }
            self.codex_send_request("turn/start", params).await?;
        }
    }

    /// Ensure provider-specific session initialization has started.
    ///
    /// For Claude, this is a no-op. For Codex app-server, this sends
    /// `initialize` and `thread/start` or `thread/resume`.
    pub async fn ensure_ready(&mut self) -> std::io::Result<()> {
        let Some(state) = self.codex_state_mut() else {
            return Ok(());
        };
        if state.initialized {
            return Ok(());
        }

        let model = state.model.clone();
        let cwd = state.cwd.clone();
        let system_prompt = state.system_prompt.clone();
        let resume_thread_id = state.resume_thread_id.clone();

        self.codex_send_request(
            "initialize",
            serde_json::json!({
                "clientInfo": {
                    "name": "midtown",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }),
        )
        .await?;

        let (start_method, start_params) = match resume_thread_id {
            Some(thread_id) => (
                "thread/resume",
                serde_json::json!({
                    "threadId": thread_id,
                    "cwd": cwd,
                    "model": model,
                    "approvalPolicy": "on-request",
                    "sandbox": "workspace-write",
                    "developerInstructions": if system_prompt.is_empty() { serde_json::Value::Null } else { serde_json::json!(system_prompt) },
                }),
            ),
            None => (
                "thread/start",
                serde_json::json!({
                    "cwd": cwd,
                    "model": model,
                    "approvalPolicy": "on-request",
                    "sandbox": "workspace-write",
                    "developerInstructions": if system_prompt.is_empty() { serde_json::Value::Null } else { serde_json::json!(system_prompt) },
                }),
            ),
        };

        let start_id = self.codex_send_request(start_method, start_params).await?;
        if let Some(state) = self.codex_state_mut() {
            state.initialized = true;
            state.start_request_id = Some(start_id);
        }

        Ok(())
    }

    /// Read the next streaming event from the session.
    ///
    /// Returns `None` when the process exits (stdout closes).
    /// Skips blank lines and unparseable lines in a loop (zero-cost,
    /// no heap allocation per skipped line).
    pub async fn next_event(&mut self) -> Option<StreamEvent> {
        match &self.protocol {
            SessionProtocol::Claude => self.next_claude_event().await,
            SessionProtocol::Codex(_) => self.next_codex_event().await,
        }
    }

    async fn next_claude_event(&mut self) -> Option<StreamEvent> {
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

    async fn next_codex_event(&mut self) -> Option<StreamEvent> {
        loop {
            let mut line = String::new();
            match self.stdout_reader.read_line(&mut line).await {
                Ok(0) => {
                    debug!("Headless codex session stdout closed");
                    return None;
                }
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let parsed: serde_json::Value = match serde_json::from_str(trimmed) {
                        Ok(value) => value,
                        Err(e) => {
                            // codex may emit non-JSON log lines on stderr in some environments.
                            warn!(
                                "Failed to parse codex app-server event: {} (line: {})",
                                e, trimmed
                            );
                            continue;
                        }
                    };
                    let (event, post_action) = match (&mut self.protocol, &mut self.session_id) {
                        (SessionProtocol::Codex(state), session_id) => {
                            codex_translate_event(&parsed, state.as_mut(), session_id)
                        }
                        (SessionProtocol::Claude, _) => (None, CodexPostAction::None),
                    };

                    if post_action == CodexPostAction::DispatchPendingTurns
                        && let Err(e) = self.codex_dispatch_pending_turns().await
                    {
                        warn!("Failed to dispatch queued codex turn: {}", e);
                    }

                    if let Some(event) = event {
                        return Some(event);
                    }
                }
                Err(e) => {
                    warn!("Error reading codex app-server stdout: {}", e);
                    return None;
                }
            }
        }
    }

    /// Send a user message to the session (for multi-turn conversations).
    ///
    /// Requires `--input-format stream-json` (which is set by default).
    pub async fn send_message(&mut self, content: &str) -> std::io::Result<()> {
        match &self.protocol {
            SessionProtocol::Claude => {
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
                debug!("Sent user message to headless Claude session");
                Ok(())
            }
            SessionProtocol::Codex(_) => {
                self.ensure_ready().await?;
                if let Some(state) = self.codex_state_mut() {
                    state.pending_messages.push_back(content.to_string());
                }
                self.codex_dispatch_pending_turns().await?;
                debug!("Queued user message for codex app-server turn");
                Ok(())
            }
        }
    }

    /// Close stdin, signaling no more input will arrive.
    ///
    /// For one-shot queries, closing stdin after sending the prompt ensures the
    /// claude process doesn't hang waiting for additional input.
    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// Get the session ID (available after the init event).
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Drain all available stderr lines without blocking.
    ///
    /// Returns a vector of stderr lines (up to a reasonable limit to avoid memory issues).
    /// This is non-blocking — reads only what's currently buffered.
    pub async fn drain_stderr(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        let mut line = String::new();

        // Read up to 100 lines or until we'd block
        for _ in 0..100 {
            line.clear();
            match tokio::time::timeout(
                Duration::from_millis(10),
                self.stderr_reader.read_line(&mut line),
            )
            .await
            {
                Ok(Ok(0)) => break, // EOF
                Ok(Ok(_)) => {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        lines.push(trimmed.to_string());
                    }
                }
                Ok(Err(_)) | Err(_) => break, // Error or timeout
            }
        }

        lines
    }

    /// Wait for the process to exit and return the exit status.
    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }

    /// Kill the child process.
    pub async fn kill(&mut self) -> std::io::Result<()> {
        self.child.kill().await
    }

    /// Check if the child process has exited without blocking.
    ///
    /// Returns `Some(ExitStatus)` if exited, `None` if still running.
    /// This is a non-blocking check using `waitpid(WNOHANG)` under the hood.
    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    /// Get the child process ID, if available.
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Mark this session to be detached instead of killed on drop.
    ///
    /// Used during daemon shutdown to allow sessions to survive restarts.
    /// The child process will die naturally from broken pipes (SIGPIPE)
    /// after the daemon drops its stdin/stdout handles. The daemon will
    /// then resume the session with `--resume <session_id>` on restart.
    pub fn detach_on_drop(&mut self) {
        self.detach_on_drop = true;
    }
}

impl Drop for HeadlessSession {
    fn drop(&mut self) {
        // Ensure the child process is killed when the session is dropped,
        // UNLESS detach_on_drop is set (for daemon restart survival).
        // tokio::process::Child does NOT kill on drop (it detaches), so we
        // must explicitly start_kill() to prevent orphaned claude processes.
        if !self.detach_on_drop {
            let _ = self.child.start_kill();
        }
    }
}

/// Execute a one-shot headless query with a timeout and return the final result.
///
/// This is a convenience function that:
/// 1. Spawns a headless session
/// 2. Sends the prompt and closes stdin (signals no more input)
/// 3. Collects all events until the result, subject to timeout
/// 4. Returns the result text and cost
///
/// On timeout, the child process is killed via the `Drop` impl on `HeadlessSession`.
///
/// For multi-turn conversations or streaming, use `HeadlessSession` directly.
pub async fn execute(
    config: &HeadlessConfig,
    prompt: &str,
    timeout: Duration,
) -> std::io::Result<HeadlessResult> {
    let mut session = HeadlessSession::spawn(config)?;

    // Send the initial prompt
    session.send_message(prompt).await?;

    // Claude stream-json one-shot flows should close stdin immediately.
    // Codex app-server may still need stdin for follow-up JSON-RPC requests
    // until the thread is initialized and the turn has been started.
    if config.auth_provider == crate::auth::AuthProvider::Claude {
        session.close_stdin();
    }

    debug!(
        "Headless: prompt sent, waiting for result (timeout={}s)",
        timeout.as_secs()
    );
    let should_wait_for_exit = config.auth_provider == crate::auth::AuthProvider::Claude;

    // Wrap event collection in a timeout. On timeout, the future is dropped,
    // which drops `session`, which calls start_kill() on the child process.
    match tokio::time::timeout(timeout, async move {
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

        if should_wait_for_exit {
            // Claude one-shot sessions exit after stdin closes.
            let _ = session.wait().await;
        } else {
            // Codex app-server is long-lived; terminate it after one-shot completion.
            let _ = session.kill().await;
        }

        HeadlessResult {
            result: result_text,
            cost_usd,
            duration_ms,
            is_error,
            session_id,
        }
    })
    .await
    {
        Ok(result) => Ok(result),
        Err(_elapsed) => {
            warn!(
                "Headless session timed out after {}s — process will be killed on drop",
                timeout.as_secs()
            );
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("headless session timed out after {}s", timeout.as_secs()),
            ))
        }
    }
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

#[path = "headless_spawn_tests.rs"]
#[cfg(test)]
mod spawn_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a minimal HeadlessConfig for testing.
    fn test_config() -> HeadlessConfig {
        HeadlessConfig {
            model: "haiku".to_string(),
            system_prompt: "You are a test assistant.".to_string(),
            json_schema: None,
            cwd: None,
            max_budget_usd: None,
            allow_tools: false,
            persist_session: false,
            resume_session_id: None,
            inactivity_timeout: None,
            team_name: None,
            agent_id: None,
            agent_name: None,
            settings_path: None,
            setting_sources: None,
            auth_provider: crate::auth::AuthProvider::Claude,
            env: std::collections::HashMap::new(),
        }
    }

    fn test_codex_state() -> CodexProtocolState {
        CodexProtocolState {
            initialized: true,
            start_request_id: Some(42),
            thread_id: None,
            turn_in_progress: true,
            next_request_id: 100,
            pending_messages: VecDeque::new(),
            latest_agent_message: None,
            resume_thread_id: None,
            model: "gpt-5-codex".to_string(),
            cwd: None,
            system_prompt: String::new(),
            output_schema: None,
        }
    }

    #[test]
    fn test_headless_config_serialization() {
        let config = HeadlessConfig {
            json_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "answer": { "type": "string" }
                },
                "required": ["answer"]
            })),
            max_budget_usd: Some(0.10),
            ..test_config()
        };

        let json = serde_json::to_string(&config).unwrap();
        let parsed: HeadlessConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.model, "haiku");
        assert!(parsed.json_schema.is_some());
        assert!(!parsed.persist_session);
        assert!(parsed.resume_session_id.is_none());
    }

    #[test]
    fn test_headless_config_persist_session_default_false() {
        // Default (from JSON without persist_session) should be false
        let json = r#"{"model":"haiku","system_prompt":"test","allow_tools":false}"#;
        let config: HeadlessConfig = serde_json::from_str(json).unwrap();
        assert!(!config.persist_session);
    }

    #[test]
    fn test_headless_config_auth_provider_default_claude() {
        let json = r#"{"model":"haiku","system_prompt":"test","allow_tools":false}"#;
        let config: HeadlessConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.auth_provider, crate::auth::AuthProvider::Claude);
    }

    #[test]
    fn test_headless_config_persist_session_true_roundtrip() {
        let config = HeadlessConfig {
            persist_session: true,
            ..test_config()
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: HeadlessConfig = serde_json::from_str(&json).unwrap();
        assert!(parsed.persist_session);
    }

    #[test]
    fn test_headless_config_resume_session_roundtrip() {
        let config = HeadlessConfig {
            resume_session_id: Some("abc-123".to_string()),
            persist_session: true,
            ..test_config()
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: HeadlessConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.resume_session_id, Some("abc-123".to_string()));
    }

    #[test]
    fn test_headless_config_inactivity_timeout_roundtrip() {
        let config = HeadlessConfig {
            inactivity_timeout: Some(Duration::from_secs(300)),
            ..test_config()
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: HeadlessConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.inactivity_timeout, Some(Duration::from_secs(300)));
    }

    #[test]
    fn test_headless_config_agent_teams_roundtrip() {
        let config = HeadlessConfig {
            team_name: Some("midtown-myrepo".to_string()),
            agent_id: Some("park@midtown-myrepo".to_string()),
            agent_name: Some("park".to_string()),
            ..test_config()
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: HeadlessConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.team_name, Some("midtown-myrepo".to_string()));
        assert_eq!(parsed.agent_id, Some("park@midtown-myrepo".to_string()));
        assert_eq!(parsed.agent_name, Some("park".to_string()));
    }

    #[test]
    fn test_headless_config_settings_path_roundtrip() {
        let config = HeadlessConfig {
            settings_path: Some("/tmp/settings.json".to_string()),
            ..test_config()
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: HeadlessConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.settings_path, Some("/tmp/settings.json".to_string()));
    }

    #[test]
    fn test_headless_config_setting_sources_roundtrip() {
        let config = HeadlessConfig {
            setting_sources: Some("project,local".to_string()),
            ..test_config()
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: HeadlessConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.setting_sources, Some("project,local".to_string()));
    }

    #[test]
    fn test_headless_config_auth_provider_roundtrip() {
        let config = HeadlessConfig {
            auth_provider: crate::auth::AuthProvider::Codex,
            ..test_config()
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: HeadlessConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.auth_provider, crate::auth::AuthProvider::Codex);
    }

    #[test]
    fn test_headless_config_backward_compat_missing_new_fields() {
        // Existing configs without new fields should deserialize with defaults
        let json = r#"{
            "model": "haiku",
            "system_prompt": "test",
            "json_schema": null,
            "cwd": null,
            "max_budget_usd": null,
            "allow_tools": false
        }"#;
        let config: HeadlessConfig = serde_json::from_str(json).unwrap();
        assert!(!config.persist_session);
        assert!(config.resume_session_id.is_none());
        assert!(config.inactivity_timeout.is_none());
        assert!(config.team_name.is_none());
        assert!(config.agent_id.is_none());
        assert!(config.agent_name.is_none());
        assert!(config.settings_path.is_none());
        assert!(config.setting_sources.is_none());
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

    #[test]
    fn test_codex_translate_start_response_emits_init_and_dispatches() {
        let mut state = test_codex_state();
        let mut session_id = None;
        let parsed = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 42,
            "result": { "thread": { "id": "thread_123" } }
        });

        let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);

        assert_eq!(session_id, Some("thread_123".to_string()));
        assert_eq!(state.thread_id, Some("thread_123".to_string()));
        assert_eq!(post_action, CodexPostAction::DispatchPendingTurns);

        match event {
            Some(StreamEvent::System {
                subtype,
                session_id,
                model,
                ..
            }) => {
                assert_eq!(subtype, "init");
                assert_eq!(session_id, Some("thread_123".to_string()));
                assert_eq!(model, Some("gpt-5-codex".to_string()));
            }
            _ => panic!("Expected codex start response to emit init system event"),
        }
    }

    #[test]
    fn test_codex_translate_start_response_error_emits_result_error() {
        let mut state = test_codex_state();
        let mut session_id = Some("existing".to_string());
        let parsed = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 42,
            "error": { "message": "start failed" }
        });

        let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);

        assert_eq!(post_action, CodexPostAction::None);
        match event {
            Some(StreamEvent::Result {
                subtype,
                is_error,
                result,
                ..
            }) => {
                assert_eq!(subtype, "error");
                assert!(is_error);
                assert_eq!(result, Some("start failed".to_string()));
            }
            _ => panic!("Expected codex start error to emit result error event"),
        }
    }

    #[test]
    fn test_codex_translate_delta_then_turn_completed_uses_accumulated_text() {
        let mut state = test_codex_state();
        let mut session_id = Some("thread_123".to_string());
        let delta = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "item/agentMessage/delta",
            "params": { "delta": "Hello" }
        });
        let turn_completed = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": { "turn": { "status": "completed" } }
        });

        let (delta_event, delta_action) =
            codex_translate_event(&delta, &mut state, &mut session_id);
        assert_eq!(delta_action, CodexPostAction::None);
        match delta_event {
            Some(StreamEvent::Assistant { .. }) => {}
            _ => panic!("Expected assistant delta event"),
        }
        assert_eq!(state.latest_agent_message, Some("Hello".to_string()));

        let (result_event, result_action) =
            codex_translate_event(&turn_completed, &mut state, &mut session_id);
        assert_eq!(result_action, CodexPostAction::DispatchPendingTurns);
        assert!(!state.turn_in_progress);
        match result_event {
            Some(StreamEvent::Result {
                subtype,
                is_error,
                result,
                ..
            }) => {
                assert_eq!(subtype, "success");
                assert!(!is_error);
                assert_eq!(result, Some("Hello".to_string()));
            }
            _ => panic!("Expected result event after turn completion"),
        }
    }
}
