//! Headless Claude Code executor using JSON streaming.
//!
//! Spawns `claude -p --verbose --output-format stream-json` as a child process
//! and communicates via stdin/stdout NDJSON. This provides a lightweight
//! executor for structured AI tasks (workflow steps,
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

use std::collections::{HashMap, VecDeque};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::OnceCell;
use tokio::sync::mpsc;
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
    /// Project name for loading sandbox configuration. Defaults to "midtown".
    #[serde(default)]
    pub project_name: Option<String>,
    /// Maximum budget in USD. Defaults to no limit.
    pub max_budget_usd: Option<f64>,
    /// Whether to allow tool use.
    ///
    /// Provider mapping:
    /// - Claude/z.ai: `--tools ""` disables all tools.
    /// - Codex:
    ///   - `allow_tools=true` -> `approvalPolicy=never`, `sandbox=danger-full-access`
    ///   - `allow_tools=false` -> `approvalPolicy=never`, `sandbox=read-only`
    ///
    /// Defaults to false (no tools).
    pub allow_tools: bool,
    /// Whether to persist the session on disk.
    ///
    /// Provider mapping:
    /// - Claude/z.ai: when false, passes `--no-session-persistence`.
    /// - Codex: no explicit non-persistent mode exists today; this setting is currently
    ///   advisory for Codex.
    ///
    /// Defaults to false (one-shot mode).
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
    /// Pre-assigned session ID for fresh sessions.
    ///
    /// When `Some`, the daemon-generated UUID is passed to the CLI as `--session-id <uuid>`
    /// so the daemon knows the session ID immediately at spawn time, without waiting for
    /// the init StreamEvent. This eliminates the race window where session-based lookups
    /// (routing, nudging) fail before the init event arrives.
    ///
    /// Only used for fresh sessions (`resume_session_id: None`). Ignored on resume
    /// sessions since `--resume <id>` already carries the known session ID.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Additional environment variables to set on the child process.
    ///
    /// Applied after the default env_remove call (MIDTOWN_AGENT), so values here
    /// take precedence. Use this to pass coworker-specific env vars like
    /// `MIDTOWN_AGENT` and provider config vars (`CLAUDE_CONFIG_DIR`/`CODEX_HOME`).
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    /// Fork the resumed session into a new independent session.
    ///
    /// When `true` (and `resume_session_id` is set), adds `--fork-session` to the
    /// Claude CLI args. The fork inherits the parent session's full context but
    /// creates a new independent session ID. Used for forked channel lead topic sessions.
    #[serde(default)]
    pub fork_session: bool,
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

/// Token usage breakdown from a Claude Code result event.
///
/// Populated from the `usage` field of the `result` stream event, which
/// mirrors the Anthropic API's usage reporting.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    /// Input tokens consumed (prompt + prior context).
    #[serde(default)]
    pub input_tokens: u64,
    /// Output tokens generated (response).
    #[serde(default)]
    pub output_tokens: u64,
    /// Prompt cache read tokens (cache hits, billed at reduced rate).
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    /// Prompt cache write tokens (cache misses that created cache entries).
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
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
        /// Token usage breakdown (input/output tokens, cache hits).
        /// Present in Claude Code stream-json result events.
        #[serde(default)]
        usage: Option<TokenUsage>,
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
    fork_session: bool,
    allow_tools: bool,
    model: String,
    cwd: Option<String>,
    system_prompt: String,
    output_schema: Option<serde_json::Value>,
    start_phase: String,
}

#[derive(Debug)]
struct CodexSessionHandle {
    token: String,
    runtime: Arc<CodexSharedRuntime>,
    event_receiver: mpsc::UnboundedReceiver<serde_json::Value>,
    stderr_cursor: u64,
}

#[derive(Debug)]
struct CodexSharedProcess {
    child: Child,
    stdin: tokio::process::ChildStdin,
}

#[derive(Debug)]
struct CodexSharedRuntime {
    process: tokio::sync::Mutex<CodexSharedProcess>,
    next_request_id: AtomicU64,
    active_sessions: AtomicUsize,
    session_senders: RwLock<HashMap<String, mpsc::UnboundedSender<serde_json::Value>>>,
    request_to_session: RwLock<HashMap<u64, String>>,
    thread_to_session: RwLock<HashMap<String, String>>,
    resume_to_session: RwLock<HashMap<String, String>>,
    stderr_lines: RwLock<VecDeque<(u64, String)>>,
    next_stderr_seq: AtomicU64,
}

static CODEX_RUNTIME: Lazy<OnceCell<Arc<CodexSharedRuntime>>> = Lazy::new(OnceCell::new);

impl CodexSharedRuntime {
    async fn spawn() -> std::io::Result<Arc<Self>> {
        let binary = crate::platform::Platform::Codex.binary_name();
        let mut cmd = Command::new(binary);

        for arg in crate::platform::build_codex_headless_args() {
            cmd.arg(arg);
        }

        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.env("DISABLE_AUTOUPDATER", "1");

        let mut child = cmd.spawn()?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("failed to capture stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("failed to capture stderr"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("failed to capture stdin"))?;

        let runtime = Arc::new(Self {
            process: tokio::sync::Mutex::new(CodexSharedProcess { child, stdin }),
            next_request_id: AtomicU64::new(1),
            active_sessions: AtomicUsize::new(0),
            session_senders: RwLock::new(HashMap::new()),
            request_to_session: RwLock::new(HashMap::new()),
            thread_to_session: RwLock::new(HashMap::new()),
            resume_to_session: RwLock::new(HashMap::new()),
            stderr_lines: RwLock::new(VecDeque::new()),
            next_stderr_seq: AtomicU64::new(0),
        });

        let runtime_for_stdout = Arc::clone(&runtime);
        tokio::spawn(async move {
            runtime_for_stdout
                .read_stdout_loop(BufReader::new(stdout))
                .await;
        });

        let runtime_for_stderr = Arc::clone(&runtime);
        tokio::spawn(async move {
            runtime_for_stderr
                .read_stderr_loop(BufReader::new(stderr))
                .await;
        });

        Ok(runtime)
    }

    async fn register_session(
        self: &Arc<Self>,
        resume_thread_id: Option<&str>,
    ) -> CodexSessionHandle {
        let token = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = mpsc::unbounded_channel();

        self.session_senders
            .write()
            .expect("session_senders lock poisoned")
            .insert(token.clone(), tx);

        if let Some(thread_id) = resume_thread_id {
            self.resume_to_session
                .write()
                .expect("resume_to_session lock poisoned")
                .insert(thread_id.to_string(), token.clone());
        }

        self.active_sessions
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);

        CodexSessionHandle {
            token,
            runtime: Arc::clone(self),
            event_receiver: rx,
            stderr_cursor: 0,
        }
    }

    fn unregister_session(&self, token: &str) {
        self.active_sessions
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        self.session_senders
            .write()
            .expect("session_senders lock poisoned")
            .remove(token);

        {
            let mut map = self
                .request_to_session
                .write()
                .expect("request_to_session lock poisoned");
            map.retain(|_, session_token| session_token != token);
        }

        {
            let mut map = self
                .thread_to_session
                .write()
                .expect("thread_to_session lock poisoned");
            map.retain(|_, session_token| session_token != token);
        }

        {
            let mut map = self
                .resume_to_session
                .write()
                .expect("resume_to_session lock poisoned");
            map.retain(|_, session_token| session_token != token);
        }
    }

    fn register_request(&self, request_id: u64, token: &str) {
        self.request_to_session
            .write()
            .expect("request_to_session lock poisoned")
            .insert(request_id, token.to_string());
    }

    fn clear_request(&self, request_id: u64) {
        self.request_to_session
            .write()
            .expect("request_to_session lock poisoned")
            .remove(&request_id);
    }

    fn next_request_id(&self) -> u64 {
        self.next_request_id.fetch_add(1, Ordering::SeqCst)
    }

    async fn send_request(
        &self,
        token: &str,
        method: &str,
        params: serde_json::Value,
    ) -> std::io::Result<u64> {
        let request_id = self.next_request_id();
        self.register_request(request_id, token);

        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params,
        });
        let mut payload = serde_json::to_string(&payload)?;
        payload.push('\n');

        let mut proc = self.process.lock().await;
        let result = proc.stdin.write_all(payload.as_bytes()).await;
        if result.is_err() {
            self.clear_request(request_id);
            return result.map(|_| request_id);
        }

        let result = proc.stdin.flush().await;
        if result.is_err() {
            self.clear_request(request_id);
            return result.map(|_| request_id);
        }

        Ok(request_id)
    }

    fn route_token_for_thread(&self, thread_id: &str) -> Option<String> {
        self.thread_to_session
            .read()
            .expect("thread_to_session lock poisoned")
            .get(thread_id)
            .cloned()
            .or_else(|| {
                self.resume_to_session
                    .read()
                    .expect("resume_to_session lock poisoned")
                    .get(thread_id)
                    .cloned()
            })
    }

    async fn dispatch_event(&self, parsed: serde_json::Value) {
        let mut routed = Vec::new();

        if let Some(id) = parsed.get("id").and_then(|v| v.as_u64()) {
            let token = self
                .request_to_session
                .write()
                .expect("request_to_session lock poisoned")
                .remove(&id);

            if let Some(token) = token.clone() {
                if let Some(thread_id) = codex_thread_id(&parsed) {
                    self.thread_to_session
                        .write()
                        .expect("thread_to_session lock poisoned")
                        .insert(thread_id, token.clone());
                    self.resume_to_session
                        .write()
                        .expect("resume_to_session lock poisoned")
                        .retain(|_, session_token| session_token != &token);
                }
                routed.push(token);
            }
        } else if let Some(thread_id) = codex_thread_id(&parsed)
            && let Some(token) = self.route_token_for_thread(&thread_id)
        {
            routed.push(token);
        }

        routed.sort();
        routed.dedup();

        if routed.is_empty() {
            return;
        }

        let senders = self
            .session_senders
            .read()
            .expect("session_senders lock poisoned");
        for token in routed {
            if let Some(tx) = senders.get(&token) {
                let _ = tx.send(parsed.clone());
            }
        }
    }

    async fn read_stdout_loop(self: Arc<Self>, mut reader: BufReader<tokio::process::ChildStdout>) {
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    debug!("Shared Codex app-server stdout closed");
                    self.session_senders
                        .write()
                        .expect("session_senders lock poisoned")
                        .clear();
                    break;
                }
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<serde_json::Value>(trimmed) {
                        Ok(parsed) => self.dispatch_event(parsed).await,
                        Err(e) => {
                            warn!(
                                "Failed to parse shared codex app-server event: {} (line: {})",
                                e, trimmed
                            );
                        }
                    }
                }
                Err(e) => {
                    warn!("Error reading shared codex app-server stdout: {}", e);
                    self.session_senders
                        .write()
                        .expect("session_senders lock poisoned")
                        .clear();
                    break;
                }
            }
        }
    }

    async fn read_stderr_loop(self: Arc<Self>, mut reader: BufReader<tokio::process::ChildStderr>) {
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let seq = self.next_stderr_seq.fetch_add(1, Ordering::SeqCst);
                    self.stderr_lines
                        .write()
                        .expect("stderr_lines lock poisoned")
                        .push_back((seq, trimmed.to_string()));
                    let mut lines = self
                        .stderr_lines
                        .write()
                        .expect("stderr_lines lock poisoned");
                    while lines.len() > 500 {
                        lines.pop_front();
                    }
                    drop(lines);
                }
                Err(e) => {
                    warn!("Error reading shared codex app-server stderr: {}", e);
                    break;
                }
            }
        }
    }

    async fn wait(&self) -> std::io::Result<std::process::ExitStatus> {
        let mut process = self.process.lock().await;
        process.child.wait().await
    }

    async fn try_wait(&self) -> std::io::Result<Option<std::process::ExitStatus>> {
        let mut process = self.process.lock().await;
        process.child.try_wait()
    }

    async fn pid(&self) -> Option<u32> {
        let process = self.process.lock().await;
        process.child.id()
    }

    #[allow(dead_code)]
    async fn shutdown(&self) {
        let mut process = self.process.lock().await;
        let _ = process.child.start_kill();
    }

    async fn drain_stderr(&self, start: u64) -> (u64, Vec<String>) {
        let (end, lines): (u64, Vec<String>) = {
            let lines = self
                .stderr_lines
                .read()
                .expect("stderr_lines lock poisoned");
            let mut values = Vec::new();
            let mut last_seq = start;
            for (seq, line) in lines.iter() {
                if *seq >= start {
                    values.push(line.clone());
                    last_seq = *seq + 1;
                }
            }
            (last_seq, values)
        };
        (end, lines)
    }
}

async fn codex_shared_runtime() -> std::io::Result<Arc<CodexSharedRuntime>> {
    CODEX_RUNTIME
        .get_or_try_init(CodexSharedRuntime::spawn)
        .await
        .map(Arc::clone)
}

pub(crate) async fn shutdown_codex_runtime() {
    if let Some(runtime) = CODEX_RUNTIME.get() {
        runtime.shutdown().await;
    }
}

fn codex_thread_id(parsed: &serde_json::Value) -> Option<String> {
    let thread_from_result = parsed.get("result").and_then(|result| result.get("thread"));
    let thread_from_params = parsed.get("params");
    let parse_thread = |thread: &serde_json::Value| {
        thread
            .get("id")
            .and_then(|id| id.as_str())
            .map(str::to_string)
    };

    thread_from_result.and_then(parse_thread).or_else(|| {
        thread_from_params.and_then(|params| {
            params
                .get("thread")
                .and_then(parse_thread)
                .or_else(|| {
                    params
                        .get("turn")
                        .and_then(|turn| turn.get("thread"))
                        .and_then(parse_thread)
                })
                .or_else(|| {
                    params
                        .get("item")
                        .and_then(|item| item.get("thread"))
                        .and_then(parse_thread)
                })
        })
    })
}

#[derive(Debug, Clone)]
struct CodexLaunchPlan {
    model: String,
    cwd: Option<String>,
    system_prompt: String,
    output_schema: Option<serde_json::Value>,
    resume_thread_id: Option<String>,
    fork_session: bool,
    allow_tools: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexPostAction {
    None,
    DispatchPendingTurns,
}

fn codex_heartbeat_event(
    session_id: &Option<String>,
    detail: serde_json::Value,
) -> Option<StreamEvent> {
    Some(StreamEvent::System {
        subtype: "heartbeat".to_string(),
        session_id: session_id.clone(),
        model: None,
        extra: serde_json::json!({
            "provider": "codex",
            "event": "heartbeat",
            "detail": detail
        }),
    })
}

fn codex_command_text(item: &serde_json::Value) -> Option<String> {
    item.get("commandActions")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|action| action.get("command"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            item.get("command")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
}

fn codex_command_call_id(item: &serde_json::Value) -> Option<String> {
    item.get("id")
        .and_then(|v| v.as_str())
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn codex_command_is_error(item: &serde_json::Value) -> bool {
    if item
        .get("exitCode")
        .and_then(|v| v.as_i64())
        .is_some_and(|code| code != 0)
    {
        return true;
    }
    item.get("status")
        .and_then(|v| v.as_str())
        .is_some_and(|status| !status.eq_ignore_ascii_case("completed"))
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
        let is_start_response = response_id == start_request_id;

        if let Some(msg) = parsed
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
        {
            let was_turn_in_progress = state.turn_in_progress;
            if !is_start_response && was_turn_in_progress {
                // Avoid deadlock: if turn/start failed, clear in-flight flag so future nudges can run.
                state.turn_in_progress = false;
            }
            let phase = if is_start_response {
                state.start_phase.clone()
            } else if was_turn_in_progress {
                "turn/start".to_string()
            } else {
                "request".to_string()
            };
            return (
                Some(StreamEvent::Result {
                    subtype: "error".to_string(),
                    is_error: true,
                    result: Some(msg.to_string()),
                    duration_ms: None,
                    total_cost_usd: None,
                    session_id: session_id.clone(),
                    usage: None,
                    extra: serde_json::json!({
                        "provider": "codex",
                        "phase": phase,
                        "request_id": response_id
                    }),
                }),
                if !is_start_response && was_turn_in_progress {
                    CodexPostAction::DispatchPendingTurns
                } else {
                    CodexPostAction::None
                },
            );
        }

        if is_start_response
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
        return (
            codex_heartbeat_event(
                session_id,
                serde_json::json!({ "kind": "rpc_response", "id": response_id }),
            ),
            CodexPostAction::None,
        );
    }

    let Some(method) = parsed.get("method").and_then(|v| v.as_str()) else {
        return (
            codex_heartbeat_event(
                session_id,
                serde_json::json!({ "kind": "event_without_method" }),
            ),
            CodexPostAction::None,
        );
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
        "item/started" => {
            if let Some(item) = params.get("item")
                && item.get("type").and_then(|t| t.as_str()) == Some("commandExecution")
                && let Some(call_id) = codex_command_call_id(item)
            {
                let command = codex_command_text(item).unwrap_or_default();
                return (
                    Some(StreamEvent::Assistant {
                        message: serde_json::json!({
                            "role": "assistant",
                            "content": [{
                                "type": "tool_use",
                                "id": call_id,
                                "name": "Bash",
                                "input": { "command": command }
                            }]
                        }),
                        session_id: session_id.clone(),
                        extra: serde_json::json!({
                            "provider": "codex",
                            "event": "item/started",
                            "item_type": "commandExecution"
                        }),
                    }),
                    CodexPostAction::None,
                );
            }
        }
        "item/completed" => {
            if let Some(item) = params.get("item")
                && item.get("type").and_then(|t| t.as_str()) == Some("commandExecution")
                && let Some(call_id) = codex_command_call_id(item)
            {
                let output = item
                    .get("aggregatedOutput")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                return (
                    Some(StreamEvent::User {
                        message: serde_json::json!({
                            "role": "user",
                            "content": [{
                                "type": "tool_result",
                                "tool_use_id": call_id,
                                "content": output,
                                "is_error": codex_command_is_error(item)
                            }]
                        }),
                        extra: serde_json::json!({
                            "provider": "codex",
                            "event": "item/completed",
                            "item_type": "commandExecution"
                        }),
                    }),
                    CodexPostAction::None,
                );
            }

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
                    usage: None,
                    extra: serde_json::json!({ "provider": "codex", "status": status }),
                }),
                CodexPostAction::DispatchPendingTurns,
            );
        }
        _ => {}
    }

    (
        codex_heartbeat_event(
            session_id,
            serde_json::json!({ "kind": "event", "method": method }),
        ),
        CodexPostAction::None,
    )
}

fn codex_thread_init_request(
    resume_thread_id: Option<&str>,
    fork_session: bool,
    allow_tools: bool,
    cwd: Option<&str>,
    model: &str,
    system_prompt: &str,
) -> (&'static str, serde_json::Value) {
    let developer_instructions = if system_prompt.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::json!(system_prompt)
    };

    // Headless sessions are non-interactive: if Codex asks for approvals,
    // turns can stall indefinitely waiting for input that will never arrive.
    // Use non-interactive approvals and disable Codex's local sandbox wrapper
    // for tool-enabled sessions (Midtown already controls sandboxing externally).
    let (approval_policy, sandbox_mode) = if allow_tools {
        ("never", "danger-full-access")
    } else {
        ("never", "read-only")
    };

    let common = |thread_id: Option<&str>| {
        let mut params = serde_json::json!({
            "cwd": cwd,
            "model": model,
            "approvalPolicy": approval_policy,
            "sandbox": sandbox_mode,
            "developerInstructions": developer_instructions,
        });
        if let Some(thread_id) = thread_id {
            params["threadId"] = serde_json::json!(thread_id);
        }
        params
    };

    match (resume_thread_id, fork_session) {
        (Some(thread_id), true) => ("thread/fork", common(Some(thread_id))),
        (Some(thread_id), false) => ("thread/resume", common(Some(thread_id))),
        (None, _) => ("thread/start", common(None)),
    }
}

fn codex_launch_plan_from_config(config: &HeadlessConfig) -> Result<CodexLaunchPlan, String> {
    // Exhaustive destructure so new HeadlessConfig fields force explicit
    // handling decisions in this platform mapper.
    let HeadlessConfig {
        model,
        system_prompt,
        json_schema,
        cwd,
        project_name: _project_name,
        max_budget_usd,
        allow_tools,
        persist_session: _persist_session,
        resume_session_id,
        inactivity_timeout: _inactivity_timeout,
        team_name: _team_name,
        agent_id: _agent_id,
        agent_name: _agent_name,
        settings_path,
        setting_sources,
        auth_provider: _auth_provider,
        session_id,
        env: _env,
        fork_session,
    } = config;

    let mut unsupported = Vec::new();
    if max_budget_usd.is_some() {
        unsupported.push("max_budget_usd");
    }
    if settings_path.is_some() {
        unsupported.push("settings_path");
    }
    if setting_sources.is_some() {
        unsupported.push("setting_sources");
    }
    if session_id.is_some() {
        unsupported.push("session_id");
    }
    if !unsupported.is_empty() {
        return Err(format!(
            "Codex headless does not support: {}",
            unsupported.join(", ")
        ));
    }

    Ok(CodexLaunchPlan {
        model: model.clone(),
        cwd: cwd.clone(),
        system_prompt: system_prompt.clone(),
        output_schema: json_schema.clone(),
        resume_thread_id: resume_session_id.clone(),
        fork_session: *fork_session,
        allow_tools: *allow_tools,
    })
}

/// A running headless Claude Code session.
///
/// Owns the child process and provides methods to read streaming events
/// and optionally send follow-up messages.
pub struct HeadlessSession {
    child: Option<Child>,
    stdout_reader: Option<BufReader<tokio::process::ChildStdout>>,
    stderr_reader: Option<BufReader<tokio::process::ChildStderr>>,
    stdin: Option<tokio::process::ChildStdin>,
    session_id: Option<String>,
    backend: HeadlessSessionBackend,
    protocol: SessionProtocol,
    codex_session: Option<CodexSessionHandle>,
    /// When true, don't kill the child process on drop (for daemon restart survival).
    detach_on_drop: bool,
}

#[derive(Debug, Clone, Copy)]
enum HeadlessSessionBackend {
    Claude,
    Codex,
}

impl HeadlessSessionBackend {
    async fn next_event(self, session: &mut HeadlessSession) -> Option<StreamEvent> {
        match self {
            Self::Claude => session.next_claude_event().await,
            Self::Codex => session.next_codex_event().await,
        }
    }

    async fn send_message(
        self,
        session: &mut HeadlessSession,
        content: &str,
    ) -> std::io::Result<()> {
        match self {
            Self::Claude => {
                let msg = serde_json::json!({
                    "type": "user",
                    "message": {
                        "role": "user",
                        "content": content
                    }
                });
                session.write_json_line(&msg).await?;
                debug!("Sent user message to headless Claude session");
                Ok(())
            }
            Self::Codex => {
                session.ensure_ready().await?;
                if let Some(state) = session.codex_state_mut() {
                    state.pending_messages.push_back(content.to_string());
                }
                session.codex_dispatch_pending_turns().await?;
                debug!("Queued user message for codex app-server turn");
                Ok(())
            }
        }
    }

    async fn wait(
        self,
        session: &mut HeadlessSession,
    ) -> std::io::Result<std::process::ExitStatus> {
        match self {
            Self::Claude => {
                let child = session
                    .child
                    .as_mut()
                    .ok_or_else(|| std::io::Error::other("no child process"))?;
                child.wait().await
            }
            Self::Codex => {
                if let Some(context) = session.codex_session() {
                    context.runtime.wait().await
                } else {
                    Err(std::io::Error::other("missing codex runtime"))
                }
            }
        }
    }

    async fn kill(self, session: &mut HeadlessSession) -> std::io::Result<()> {
        match self {
            Self::Claude => {
                if let Some(child) = session.child.as_mut() {
                    child.kill().await
                } else {
                    Ok(())
                }
            }
            Self::Codex => Ok(()),
        }
    }

    fn try_wait(
        self,
        session: &mut HeadlessSession,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        match self {
            Self::Claude => {
                let child = session
                    .child
                    .as_mut()
                    .ok_or_else(|| std::io::Error::other("no child process"))?;
                child.try_wait()
            }
            Self::Codex => {
                if let Some(context) = session.codex_session() {
                    futures::executor::block_on(context.runtime.try_wait())
                } else {
                    Err(std::io::Error::other("missing codex runtime"))
                }
            }
        }
    }

    fn pid(self, session: &HeadlessSession) -> Option<u32> {
        match self {
            Self::Claude => session.child.as_ref().and_then(|child| child.id()),
            Self::Codex => session
                .codex_session()
                .and_then(|context| futures::executor::block_on(context.runtime.pid())),
        }
    }

    async fn drain_stderr(self, session: &mut HeadlessSession) -> Vec<String> {
        match self {
            Self::Claude => {
                let mut lines = Vec::new();
                let mut line = String::new();
                let reader = session
                    .stderr_reader
                    .as_mut()
                    .expect("missing claude stderr reader");

                for _ in 0..100 {
                    line.clear();
                    match tokio::time::timeout(
                        Duration::from_millis(10),
                        reader.read_line(&mut line),
                    )
                    .await
                    {
                        Ok(Ok(0)) => break,
                        Ok(Ok(_)) => {
                            let trimmed = line.trim();
                            if !trimmed.is_empty() {
                                lines.push(trimmed.to_string());
                            }
                        }
                        Ok(Err(_)) | Err(_) => break,
                    }
                }

                lines
            }
            Self::Codex => {
                if let Some(context) = session.codex_session() {
                    let (next_cursor, lines) =
                        context.runtime.drain_stderr(context.stderr_cursor).await;
                    if let Some(context_mut) = session.codex_session_mut() {
                        context_mut.stderr_cursor = next_cursor;
                    }
                    return lines;
                }

                Vec::new()
            }
        }
    }

    fn close_stdin(self, session: &mut HeadlessSession) {
        if matches!(self, Self::Claude) {
            session.stdin = None;
        }
    }

    fn should_wait_for_exit(self) -> bool {
        matches!(self, Self::Claude)
    }
}

struct ClaudeHeadlessAdapter;
struct CodexHeadlessAdapter;

impl CodexHeadlessAdapter {
    async fn spawn(config: &HeadlessConfig) -> std::io::Result<HeadlessSession> {
        let plan = codex_launch_plan_from_config(config)
            .map_err(|msg| std::io::Error::new(std::io::ErrorKind::InvalidInput, msg))?;

        let protocol = SessionProtocol::Codex(Box::new(CodexProtocolState {
            initialized: false,
            start_request_id: None,
            thread_id: None,
            turn_in_progress: false,
            next_request_id: 1,
            pending_messages: VecDeque::new(),
            latest_agent_message: None,
            resume_thread_id: plan.resume_thread_id,
            fork_session: plan.fork_session,
            allow_tools: plan.allow_tools,
            model: plan.model,
            cwd: plan.cwd,
            system_prompt: plan.system_prompt,
            output_schema: plan.output_schema,
            start_phase: "thread/start".to_string(),
        }));

        let resume_thread_id = match &protocol {
            SessionProtocol::Codex(state) => state.resume_thread_id.clone(),
            _ => None,
        };

        let runtime = codex_shared_runtime().await?;
        let codex_session = Some(runtime.register_session(resume_thread_id.as_deref()).await);

        info!(
            "Spawned shared codex headless session (model={}, resume={})",
            config.model,
            config.resume_session_id.is_some()
        );

        Ok(HeadlessSession {
            child: None,
            stdout_reader: None,
            stderr_reader: None,
            stdin: None,
            session_id: None,
            backend: HeadlessSessionBackend::Codex,
            protocol,
            codex_session,
            detach_on_drop: false,
        })
    }
}

impl ClaudeHeadlessAdapter {
    async fn spawn(config: &HeadlessConfig) -> std::io::Result<HeadlessSession> {
        let platform = crate::platform::Platform::from_provider(config.auth_provider);
        let binary = platform.binary_name();

        let cli_args = crate::platform::build_claude_headless_args(config);

        let primary_repo = config
            .cwd
            .as_deref()
            .map(std::path::Path::new)
            .unwrap_or(std::path::Path::new("/tmp"));
        let project_name = config.project_name.as_deref().unwrap_or("midtown");
        let sandbox_config = crate::config::get_project_sandbox_config(project_name);
        let writable =
            crate::sandbox::writable_dirs(primary_repo, &[], &sandbox_config.allowed_paths);

        let mut cmd = if cfg!(target_os = "macos") {
            match crate::sandbox::sandbox_exec_prefix(&writable) {
                Ok((_profile_path, prefix)) => {
                    let mut c = Command::new("sandbox-exec");
                    for arg in &prefix {
                        c.arg(arg);
                    }
                    c.arg(binary);
                    c
                }
                Err(e) => {
                    warn!("Sandbox setup failed, running without sandbox: {}", e);
                    Command::new(binary)
                }
            }
        } else if cfg!(target_os = "linux") && crate::sandbox::bwrap_available() {
            let mut c = Command::new("bwrap");
            c.args(["--ro-bind", "/", "/"]);
            for dir in &writable {
                c.args(["--bind", dir, dir]);
            }
            c.args(["--dev", "/dev", "--proc", "/proc", "--", binary]);
            c
        } else {
            Command::new(binary)
        };

        for arg in &cli_args {
            cmd.arg(arg);
        }

        if let Some(ref cwd) = config.cwd {
            cmd.current_dir(cwd);
        }

        cmd.env_remove("MIDTOWN_AGENT");
        cmd.env_remove("CLAUDECODE");
        cmd.env("DISABLE_AUTOUPDATER", "1");

        if config.team_name.is_some() && config.auth_provider == crate::auth::AuthProvider::Claude {
            cmd.env("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS", "1");
        }

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

        info!(
            "Spawned headless {:?} session (model={}, resume={})",
            config.auth_provider,
            config.model,
            config.resume_session_id.is_some()
        );

        Ok(HeadlessSession {
            child: Some(child),
            stdout_reader: Some(stdout_reader),
            stderr_reader: Some(stderr_reader),
            stdin,
            session_id: None,
            backend: HeadlessSessionBackend::Claude,
            protocol: SessionProtocol::Claude,
            codex_session: None,
            detach_on_drop: false,
        })
    }
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
    pub async fn spawn(config: &HeadlessConfig) -> std::io::Result<Self> {
        if let Err(e) = crate::platform_launch::run_platform_prelaunch_hook(config.auth_provider) {
            warn!("Platform pre-launch hook failed (continuing): {}", e);
        }

        match config.auth_provider {
            crate::auth::AuthProvider::Codex => CodexHeadlessAdapter::spawn(config).await,
            crate::auth::AuthProvider::Claude | crate::auth::AuthProvider::Zai => {
                ClaudeHeadlessAdapter::spawn(config).await
            }
        }
    }

    /// Convenience method to spawn a session that resumes a previous session.
    ///
    /// Creates a config with `resume_session_id` set and `persist_session: true`,
    /// clears `system_prompt` and `json_schema` (not used in resume mode).
    pub async fn resume(session_id: &str, base_config: &HeadlessConfig) -> std::io::Result<Self> {
        let config = HeadlessConfig {
            resume_session_id: Some(session_id.to_string()),
            persist_session: true,
            system_prompt: String::new(), // Not used in resume mode
            json_schema: None,            // Not used in resume mode
            ..base_config.clone()
        };
        Self::spawn(&config).await
    }

    fn codex_state_mut(&mut self) -> Option<&mut CodexProtocolState> {
        match &mut self.protocol {
            SessionProtocol::Codex(state) => Some(state.as_mut()),
            SessionProtocol::Claude => None,
        }
    }

    fn codex_session_mut(&mut self) -> Option<&mut CodexSessionHandle> {
        self.codex_session.as_mut()
    }

    fn codex_session(&self) -> Option<&CodexSessionHandle> {
        self.codex_session.as_ref()
    }

    pub fn is_codex_session(&self) -> bool {
        matches!(self.backend, HeadlessSessionBackend::Codex)
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
        let token = self
            .codex_session()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing codex runtime")
            })?
            .token
            .clone();

        let runtime = self
            .codex_session()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing codex runtime")
            })?
            .runtime
            .as_ref();

        let request_id = runtime.send_request(&token, method, params).await?;
        if let Some(state) = self.codex_state_mut() {
            state.next_request_id = state.next_request_id.saturating_add(1);
        }
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
            if let Err(e) = self.codex_send_request("turn/start", params).await {
                if let Some(state) = self.codex_state_mut() {
                    state.turn_in_progress = false;
                    state.pending_messages.push_front(prompt);
                }
                return Err(e);
            }
        }
    }

    /// Ensure provider-specific session initialization has started.
    ///
    /// For Claude, this is a no-op. For Codex app-server, this sends
    /// `initialize` and one of `thread/start`, `thread/resume`, or `thread/fork`.
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
        let fork_session = state.fork_session;
        let allow_tools = state.allow_tools;

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

        let (start_method, start_params) = codex_thread_init_request(
            resume_thread_id.as_deref(),
            fork_session,
            allow_tools,
            cwd.as_deref(),
            &model,
            &system_prompt,
        );

        let start_id = self.codex_send_request(start_method, start_params).await?;
        if let Some(state) = self.codex_state_mut() {
            state.initialized = true;
            state.start_request_id = Some(start_id);
            state.start_phase = start_method.to_string();
        }

        Ok(())
    }

    /// Read the next streaming event from the session.
    ///
    /// Returns `None` when the process exits (stdout closes).
    /// Skips blank lines and unparseable lines in a loop (zero-cost,
    /// no heap allocation per skipped line).
    pub async fn next_event(&mut self) -> Option<StreamEvent> {
        self.backend.next_event(self).await
    }

    async fn next_claude_event(&mut self) -> Option<StreamEvent> {
        loop {
            let mut line = String::new();
            let reader = self
                .stdout_reader
                .as_mut()
                .expect("missing claude stdout reader");
            match reader.read_line(&mut line).await {
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
            let parsed = {
                let receiver = match self.codex_session_mut() {
                    Some(context) => &mut context.event_receiver,
                    None => return None,
                };

                receiver.recv().await?
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
    }

    /// Send a user message to the session (for multi-turn conversations).
    ///
    /// Requires `--input-format stream-json` (which is set by default).
    pub async fn send_message(&mut self, content: &str) -> std::io::Result<()> {
        self.backend.send_message(self, content).await
    }

    /// Close stdin, signaling no more input will arrive.
    ///
    /// For one-shot queries, closing stdin after sending the prompt ensures the
    /// claude process doesn't hang waiting for additional input.
    pub fn close_stdin(&mut self) {
        self.backend.close_stdin(self)
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
        self.backend.drain_stderr(self).await
    }

    /// Wait for the process to exit and return the exit status.
    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.backend.wait(self).await
    }

    /// Kill the child process.
    pub async fn kill(&mut self) -> std::io::Result<()> {
        self.backend.kill(self).await
    }

    /// Check if the child process has exited without blocking.
    ///
    /// Returns `Some(ExitStatus)` if exited, `None` if still running.
    /// This is a non-blocking check using `waitpid(WNOHANG)` under the hood.
    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.backend.try_wait(self)
    }

    /// Get the child process ID, if available.
    pub fn pid(&self) -> Option<u32> {
        self.backend.pid(self)
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

    pub fn should_wait_for_exit_on_result(&self) -> bool {
        self.backend.should_wait_for_exit()
    }
}

impl Drop for HeadlessSession {
    fn drop(&mut self) {
        if let Some(context) = self.codex_session.take() {
            context.runtime.unregister_session(&context.token);
        }

        if !self.detach_on_drop
            && let Some(child) = self.child.as_mut()
        {
            let _ = child.start_kill();
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
    let mut session = HeadlessSession::spawn(config).await?;

    // Send the initial prompt
    session.send_message(prompt).await?;

    // Claude stream-json one-shot flows should close stdin immediately.
    // Codex app-server keeps the shared process alive for additional turns.
    if session.should_wait_for_exit_on_result() {
        session.close_stdin();
    }

    debug!(
        "Headless: prompt sent, waiting for result (timeout={}s)",
        timeout.as_secs()
    );
    let should_wait_for_exit = session.should_wait_for_exit_on_result();

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

#[path = "headless_fuzz_tests.rs"]
#[cfg(test)]
mod fuzz_tests;

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
            project_name: Some("midtown".to_string()),
            max_budget_usd: None,
            allow_tools: false,
            persist_session: false,
            resume_session_id: None,
            session_id: None,
            inactivity_timeout: None,
            team_name: None,
            agent_id: None,
            agent_name: None,
            settings_path: None,
            setting_sources: None,
            auth_provider: crate::auth::AuthProvider::Claude,
            env: std::collections::BTreeMap::new(),
            fork_session: false,
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
            fork_session: false,
            allow_tools: true,
            model: "gpt-5-codex".to_string(),
            cwd: None,
            system_prompt: String::new(),
            output_schema: None,
            start_phase: "thread/start".to_string(),
        }
    }

    fn test_claude_session() -> HeadlessSession {
        HeadlessSession {
            child: None,
            stdout_reader: None,
            stderr_reader: None,
            stdin: None,
            session_id: None,
            backend: HeadlessSessionBackend::Claude,
            protocol: SessionProtocol::Claude,
            codex_session: None,
            detach_on_drop: false,
        }
    }

    fn test_codex_session() -> HeadlessSession {
        HeadlessSession {
            child: None,
            stdout_reader: None,
            stderr_reader: None,
            stdin: None,
            session_id: None,
            backend: HeadlessSessionBackend::Codex,
            protocol: SessionProtocol::Codex(Box::new(test_codex_state())),
            codex_session: None,
            detach_on_drop: false,
        }
    }

    #[test]
    fn test_headless_session_protocol_flags() {
        let claude_session = test_claude_session();
        let codex_session = test_codex_session();

        assert!(!claude_session.is_codex_session());
        assert!(codex_session.is_codex_session());

        assert!(claude_session.should_wait_for_exit_on_result());
        assert!(!codex_session.should_wait_for_exit_on_result());
    }

    #[tokio::test]
    async fn test_codex_session_runtime_methods_require_runtime() {
        let mut session = test_codex_session();
        session
            .codex_state_mut()
            .expect("expected codex protocol")
            .thread_id = Some("thread-1".to_string());
        session
            .codex_state_mut()
            .expect("expected codex protocol")
            .initialized = false;

        assert!(session.next_event().await.is_none());
        assert!(session.drain_stderr().await.is_empty());
        assert!(session.kill().await.is_ok());
        assert_eq!(session.pid(), None);
        assert_eq!(
            session.wait().await.err().unwrap().to_string(),
            "missing codex runtime".to_string()
        );
        assert_eq!(
            session.try_wait().err().unwrap().to_string(),
            "missing codex runtime".to_string()
        );
        assert_eq!(
            session
                .send_message("hello")
                .await
                .err()
                .unwrap()
                .to_string(),
            "missing codex runtime".to_string()
        );

        session.close_stdin();
        assert!(session.stdin.is_none());
    }

    #[tokio::test]
    async fn test_shutdown_codex_runtime_noop_when_not_started() {
        shutdown_codex_runtime().await;
    }

    #[test]
    fn test_codex_launch_plan_rejects_unsupported_fields() {
        let config = HeadlessConfig {
            auth_provider: crate::auth::AuthProvider::Codex,
            max_budget_usd: Some(1.0),
            settings_path: Some("/tmp/settings.json".to_string()),
            setting_sources: Some("project,local".to_string()),
            session_id: Some("session-123".to_string()),
            ..test_config()
        };

        let error = codex_launch_plan_from_config(&config).unwrap_err();
        assert!(
            error.contains("max_budget_usd")
                && error.contains("settings_path")
                && error.contains("setting_sources")
                && error.contains("session_id")
        );
    }

    #[test]
    fn test_codex_launch_plan_accepts_supported_fields() {
        let config = HeadlessConfig {
            auth_provider: crate::auth::AuthProvider::Codex,
            model: "gpt-5.3-codex".to_string(),
            system_prompt: "System".to_string(),
            json_schema: Some(serde_json::json!({"type":"object"})),
            cwd: Some("/tmp/project".to_string()),
            resume_session_id: Some("thread-parent".to_string()),
            fork_session: true,
            allow_tools: false,
            ..test_config()
        };

        let plan = codex_launch_plan_from_config(&config).unwrap();
        assert_eq!(plan.model, "gpt-5.3-codex");
        assert_eq!(plan.system_prompt, "System");
        assert_eq!(plan.cwd, Some("/tmp/project".to_string()));
        assert_eq!(plan.resume_thread_id, Some("thread-parent".to_string()));
        assert!(plan.fork_session);
        assert!(!plan.allow_tools);
        assert!(plan.output_schema.is_some());
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
    fn test_codex_translate_turn_start_error_clears_in_flight_turn() {
        let mut state = test_codex_state();
        state.start_request_id = Some(42);
        state.turn_in_progress = true;
        let mut session_id = Some("thread_123".to_string());
        let parsed = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 99,
            "error": { "message": "turn failed" }
        });

        let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);

        assert_eq!(post_action, CodexPostAction::DispatchPendingTurns);
        assert!(!state.turn_in_progress);
        match event {
            Some(StreamEvent::Result {
                subtype,
                is_error,
                result,
                extra,
                ..
            }) => {
                assert_eq!(subtype, "error");
                assert!(is_error);
                assert_eq!(result, Some("turn failed".to_string()));
                assert_eq!(extra["phase"], "turn/start");
                assert_eq!(extra["request_id"], 99);
            }
            _ => panic!("Expected codex turn error to emit result error event"),
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

    #[test]
    fn test_codex_translate_unknown_event_emits_heartbeat() {
        let mut state = test_codex_state();
        let mut session_id = Some("thread_123".to_string());
        let parsed = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "item/thinking/delta",
            "params": { "delta": "..." }
        });

        let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);
        assert_eq!(post_action, CodexPostAction::None);
        match event {
            Some(StreamEvent::System { subtype, extra, .. }) => {
                assert_eq!(subtype, "heartbeat");
                assert_eq!(extra["provider"], "codex");
                assert_eq!(extra["event"], "heartbeat");
                assert_eq!(extra["detail"]["method"], "item/thinking/delta");
            }
            _ => panic!("Expected codex unknown event to emit heartbeat system event"),
        }
    }

    #[test]
    fn test_codex_translate_command_execution_started_emits_tool_use() {
        let mut state = test_codex_state();
        let mut session_id = Some("thread_123".to_string());
        let parsed = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "item/started",
            "params": {
                "item": {
                    "type": "commandExecution",
                    "id": "call_abc",
                    "commandActions": [{"type": "unknown", "command": "pwd"}]
                }
            }
        });

        let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);
        assert_eq!(post_action, CodexPostAction::None);
        match event {
            Some(StreamEvent::Assistant { message, extra, .. }) => {
                assert_eq!(extra["provider"], "codex");
                assert_eq!(extra["event"], "item/started");
                assert_eq!(message["role"], "assistant");
                assert_eq!(message["content"][0]["type"], "tool_use");
                assert_eq!(message["content"][0]["id"], "call_abc");
                assert_eq!(message["content"][0]["name"], "Bash");
                assert_eq!(message["content"][0]["input"]["command"], "pwd");
            }
            _ => panic!("Expected commandExecution start to emit assistant tool_use"),
        }
    }

    #[test]
    fn test_codex_translate_command_execution_completed_emits_tool_result() {
        let mut state = test_codex_state();
        let mut session_id = Some("thread_123".to_string());
        let parsed = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "item/completed",
            "params": {
                "item": {
                    "type": "commandExecution",
                    "id": "call_abc",
                    "aggregatedOutput": "/tmp\n",
                    "exitCode": 0,
                    "status": "completed"
                }
            }
        });

        let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);
        assert_eq!(post_action, CodexPostAction::None);
        match event {
            Some(StreamEvent::User { message, extra, .. }) => {
                assert_eq!(extra["provider"], "codex");
                assert_eq!(extra["event"], "item/completed");
                assert_eq!(message["role"], "user");
                assert_eq!(message["content"][0]["type"], "tool_result");
                assert_eq!(message["content"][0]["tool_use_id"], "call_abc");
                assert_eq!(message["content"][0]["content"], "/tmp\n");
                assert_eq!(message["content"][0]["is_error"], false);
            }
            _ => panic!("Expected commandExecution completion to emit user tool_result"),
        }
    }

    #[test]
    fn test_codex_translate_command_execution_failed_sets_tool_result_error() {
        let mut state = test_codex_state();
        let mut session_id = Some("thread_123".to_string());
        let parsed = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "item/completed",
            "params": {
                "item": {
                    "type": "commandExecution",
                    "id": "call_abc",
                    "aggregatedOutput": "boom\\n",
                    "exitCode": 1,
                    "status": "failed"
                }
            }
        });

        let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);
        assert_eq!(post_action, CodexPostAction::None);
        match event {
            Some(StreamEvent::User { message, .. }) => {
                assert_eq!(message["content"][0]["is_error"], true);
            }
            _ => panic!("Expected commandExecution failure to emit user tool_result"),
        }
    }

    #[test]
    fn test_codex_thread_init_request_selects_fork_for_resume_fork() {
        let (method, params) = codex_thread_init_request(
            Some("thread_parent"),
            true,
            true,
            Some("/tmp/project"),
            "gpt-5.3-codex",
            "system prompt",
        );

        assert_eq!(method, "thread/fork");
        assert_eq!(params["threadId"], "thread_parent");
        assert_eq!(params["cwd"], "/tmp/project");
        assert_eq!(params["model"], "gpt-5.3-codex");
        assert_eq!(params["approvalPolicy"], "never");
        assert_eq!(params["sandbox"], "danger-full-access");
        assert_eq!(params["developerInstructions"], "system prompt");
    }

    #[test]
    fn test_codex_thread_init_request_selects_resume_without_fork() {
        let (method, params) = codex_thread_init_request(
            Some("thread_parent"),
            false,
            true,
            Some("/tmp/project"),
            "gpt-5.3-codex",
            "",
        );

        assert_eq!(method, "thread/resume");
        assert_eq!(params["threadId"], "thread_parent");
        assert_eq!(params["developerInstructions"], serde_json::Value::Null);
    }

    #[test]
    fn test_codex_thread_init_request_selects_start_when_not_resuming() {
        let (method, params) =
            codex_thread_init_request(None, true, true, None, "gpt-5.3-codex", "system prompt");

        assert_eq!(method, "thread/start");
        assert_eq!(params.get("threadId"), None);
        assert_eq!(params["cwd"], serde_json::Value::Null);
        assert_eq!(params["model"], "gpt-5.3-codex");
    }

    #[test]
    fn test_codex_thread_init_request_disables_tools_when_allow_tools_false() {
        let (method, params) = codex_thread_init_request(
            None,
            false,
            false,
            Some("/tmp/project"),
            "gpt-5.3-codex",
            "system prompt",
        );

        assert_eq!(method, "thread/start");
        assert_eq!(params["approvalPolicy"], "never");
        assert_eq!(params["sandbox"], "read-only");
    }
}
