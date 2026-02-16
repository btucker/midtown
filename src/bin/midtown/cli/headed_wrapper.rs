//! Headed wrapper CLI.
//!
//! Provides an adapter-neutral intercom loop for headed sessions:
//! register -> poll -> deliver -> ack.

use std::io::{Read, Write};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{collections::VecDeque, mem};

use crate::cli::Response;
use crate::client::DaemonClient;
use tracing::{debug, info};

const COWORKER_INPUT_MAX_WAIT: Duration = Duration::from_secs(120);
const COWORKER_INPUT_STABLE_DURATION: Duration = Duration::from_secs(20);
const NUDGE_POLL_INTERVAL: Duration = Duration::from_secs(3);
const SUBMIT_RETRY_ATTEMPTS: usize = 3;
const SUBMIT_RETRY_DELAY: Duration = Duration::from_millis(200);
const SUBMIT_PROCESS_DELAY: Duration = Duration::from_millis(120);
const PAYLOAD_SETTLE_DELAY: Duration = Duration::from_millis(80);
const OUTPUT_MIRROR_MAX_BYTES: usize = 256 * 1024;
const OUTPUT_MIRROR_MAX_LINES: usize = 500;

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum WrapperProviderArg {
    Claude,
    Codex,
    Zai,
}

impl From<WrapperProviderArg> for midtown::auth::AuthProvider {
    fn from(value: WrapperProviderArg) -> Self {
        match value {
            WrapperProviderArg::Claude => midtown::auth::AuthProvider::Claude,
            WrapperProviderArg::Codex => midtown::auth::AuthProvider::Codex,
            WrapperProviderArg::Zai => midtown::auth::AuthProvider::Zai,
        }
    }
}

#[derive(Clone, clap::Subcommand)]
pub enum HeadedWrapperCommand {
    /// Start wrapper loop (register + poll + deliver + ack)
    Run {
        /// Session key (e.g., lead, park)
        #[arg(long, default_value = "lead")]
        session: String,
        /// Adapter/provider for payload shaping
        #[arg(long, value_enum, default_value_t = WrapperProviderArg::Claude)]
        provider: WrapperProviderArg,
        /// Stable adapter identifier (defaults to process-based id)
        #[arg(long)]
        adapter_id: Option<String>,
        /// Poll interval in milliseconds
        #[arg(long, default_value_t = 500)]
        poll_interval_ms: u64,
        /// Max messages per poll
        #[arg(long, default_value_t = 50)]
        batch_limit: usize,
        /// Process one poll cycle and exit
        #[arg(long)]
        once: bool,
        /// Optional shell command run for each delivered payload.
        ///
        /// The wrapper sets env vars:
        /// - MIDTOWN_HEADED_PAYLOAD
        /// - MIDTOWN_HEADED_SESSION
        /// - MIDTOWN_HEADED_MESSAGE_ID
        /// - MIDTOWN_HEADED_KIND
        /// - MIDTOWN_HEADED_SUBMIT
        #[arg(long)]
        on_message_cmd: Option<String>,
    },
    /// Run an interactive agent process under a PTY and inject nudges via RPC.
    ///
    /// This is the canonical headed intercom path: the wrapper owns the agent
    /// process, polls daemon nudges, and writes them into the child PTY input.
    RunAgent {
        /// Session key (e.g., lead, park)
        #[arg(long)]
        session: String,
        /// Adapter/provider for payload shaping
        #[arg(long, value_enum)]
        provider: WrapperProviderArg,
        /// Stable adapter identifier (defaults to process-based id)
        #[arg(long)]
        adapter_id: Option<String>,
        /// Poll interval in milliseconds
        #[arg(long, default_value_t = 500)]
        poll_interval_ms: u64,
        /// Max messages per poll
        #[arg(long, default_value_t = 50)]
        batch_limit: usize,
        /// Working directory for the agent process
        #[arg(long)]
        cwd: Option<String>,
        /// Agent command to execute under PTY (use `--` before command)
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<String>,
    },
    /// Register adapter lease for a session
    Register {
        #[arg(long)]
        session: String,
        #[arg(long)]
        adapter_id: String,
        #[arg(long, value_enum, default_value_t = WrapperProviderArg::Claude)]
        provider: WrapperProviderArg,
    },
    /// Poll queued messages
    Poll {
        #[arg(long)]
        session: String,
        #[arg(long)]
        adapter_id: String,
        #[arg(long, default_value_t = 0)]
        after_id: u64,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Ack messages up to msg_id
    Ack {
        #[arg(long)]
        session: String,
        #[arg(long)]
        adapter_id: String,
        #[arg(long)]
        msg_id: u64,
    },
    /// Unregister adapter lease
    Unregister {
        #[arg(long)]
        session: String,
        #[arg(long)]
        adapter_id: String,
    },
}

#[derive(Debug, serde::Deserialize)]
struct HeadedPollMessage {
    id: u64,
    kind: String,
    text: String,
    submit: bool,
}

#[derive(Debug, serde::Deserialize)]
struct HeadedPollResult {
    messages: Vec<HeadedPollMessage>,
    #[serde(default)]
    capture_output: bool,
}

fn default_adapter_id(session: &str) -> String {
    format!("midtown-wrapper-{}-{}", session, std::process::id())
}

type SharedWriter = Arc<Mutex<Box<dyn std::io::Write + Send>>>;
type SharedInputState = Arc<Mutex<InputState>>;
type SharedOutputMirror = Arc<Mutex<OutputMirror>>;

#[cfg(test)]
#[derive(Debug, Clone)]
struct InputSnapshot {
    current_input: String,
}

#[derive(Debug)]
struct InputState {
    current_input: String,
    last_change: Instant,
    in_escape_sequence: bool,
}

impl Default for InputState {
    fn default() -> Self {
        Self {
            current_input: String::new(),
            last_change: Instant::now(),
            in_escape_sequence: false,
        }
    }
}

#[derive(Debug, Default)]
struct OutputMirror {
    bytes: VecDeque<u8>,
}

impl OutputMirror {
    fn ingest(&mut self, chunk: &[u8]) {
        self.bytes.extend(chunk.iter().copied());
        while self.bytes.len() > OUTPUT_MIRROR_MAX_BYTES {
            self.bytes.pop_front();
        }
    }

    fn sanitized_content(&self) -> String {
        let raw: Vec<u8> = self.bytes.iter().copied().collect();
        sanitize_terminal_text(&raw)
    }
}

fn write_payload_to_pty(writer: &SharedWriter, payload: &str) -> Result<(), String> {
    let mut guard = writer
        .lock()
        .map_err(|_| "Failed to lock PTY writer".to_string())?;
    guard
        .write_all(payload.as_bytes())
        .map_err(|e| format!("Failed writing payload to PTY: {}", e))?;
    guard
        .flush()
        .map_err(|e| format!("Failed flushing payload to PTY: {}", e))
}

fn write_submit_to_pty(writer: &SharedWriter) -> Result<(), String> {
    let mut guard = writer
        .lock()
        .map_err(|_| "Failed to lock PTY writer".to_string())?;
    guard
        .write_all(b"\r")
        .map_err(|e| format!("Failed writing submit key to PTY: {}", e))?;
    guard
        .flush()
        .map_err(|e| format!("Failed flushing submit key to PTY: {}", e))
}

fn update_input_state(state: &SharedInputState, bytes: &[u8]) {
    let mut guard = match state.lock() {
        Ok(guard) => guard,
        Err(_) => return,
    };

    let mut changed = false;
    for &byte in bytes {
        if guard.in_escape_sequence {
            if (0x40..=0x7e).contains(&byte) {
                guard.in_escape_sequence = false;
            }
            continue;
        }

        match byte {
            b'\x1b' => {
                guard.in_escape_sequence = true;
            }
            b'\r' | b'\n' => {
                if !guard.current_input.is_empty() {
                    guard.current_input.clear();
                    changed = true;
                }
            }
            b'\x08' | b'\x7f' => {
                if guard.current_input.pop().is_some() {
                    changed = true;
                }
            }
            b'\x15' => {
                if !guard.current_input.is_empty() {
                    guard.current_input.clear();
                    changed = true;
                }
            }
            _ => {
                if byte.is_ascii_control() {
                    continue;
                }
                guard.current_input.push(byte as char);
                changed = true;
            }
        }
    }

    if changed {
        guard.last_change = Instant::now();
    }
}

#[cfg(test)]
fn snapshot_input_state(state: &SharedInputState) -> InputSnapshot {
    match state.lock() {
        Ok(guard) => InputSnapshot {
            current_input: guard.current_input.clone(),
        },
        Err(_) => InputSnapshot {
            current_input: String::new(),
        },
    }
}

fn spawn_stdin_relay(writer: SharedWriter, input_state: SharedInputState) {
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        let mut buf = [0u8; 4096];
        loop {
            match input.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    update_input_state(&input_state, &buf[..n]);
                    let mut guard = match writer.lock() {
                        Ok(guard) => guard,
                        Err(_) => break,
                    };
                    if guard.write_all(&buf[..n]).is_err() {
                        break;
                    }
                    let _ = guard.flush();
                }
                Err(_) => break,
            }
        }
    });
}

fn spawn_stdout_relay(
    mut reader: Box<dyn std::io::Read + Send>,
    output_mirror: SharedOutputMirror,
) {
    std::thread::spawn(move || {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut mirror) = output_mirror.lock() {
                        mirror.ingest(&buf[..n]);
                    }
                    if out.write_all(&buf[..n]).is_err() {
                        break;
                    }
                    if out.flush().is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
}

fn capture_output_text(mirror: &SharedOutputMirror) -> String {
    match mirror.lock() {
        Ok(mirror) => mirror.sanitized_content(),
        Err(_) => String::new(),
    }
}

fn sanitize_terminal_text(raw: &[u8]) -> String {
    let decoded = String::from_utf8_lossy(raw);
    let mut lines: VecDeque<String> = VecDeque::new();
    let mut current_line = String::new();
    let mut in_escape_sequence = false;

    for ch in decoded.chars() {
        if in_escape_sequence {
            if ('@'..='~').contains(&ch) {
                in_escape_sequence = false;
            }
            continue;
        }

        match ch {
            '\u{1b}' => {
                in_escape_sequence = true;
            }
            '\r' => {
                current_line.clear();
            }
            '\n' => {
                lines.push_back(mem::take(&mut current_line));
                while lines.len() > OUTPUT_MIRROR_MAX_LINES {
                    lines.pop_front();
                }
            }
            '\u{8}' => {
                current_line.pop();
            }
            c if c.is_control() => {}
            c => current_line.push(c),
        }
    }

    if !current_line.is_empty() {
        lines.push_back(current_line);
    }
    while lines.len() > OUTPUT_MIRROR_MAX_LINES {
        lines.pop_front();
    }
    lines.into_iter().collect::<Vec<_>>().join("\n")
}

fn apply_submit_key(mut payload: String, submit: bool) -> String {
    if !submit {
        return payload;
    }

    // PTYs expect Enter as carriage return (`\r`), not newline (`\n`).
    while payload.ends_with('\n') || payload.ends_with('\r') {
        payload.pop();
    }
    payload.push('\r');
    payload
}

fn trim_trailing_linebreaks(mut payload: String) -> String {
    while payload.ends_with('\n') || payload.ends_with('\r') {
        payload.pop();
    }
    payload
}

fn get_input_text(content: &str) -> Option<String> {
    let prompt_line = content
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .find(|line| line.contains('❯'));

    if let Some(line) = prompt_line
        && let Some(pos) = line.find('❯')
    {
        let after_prompt = line[pos + '❯'.len_utf8()..].trim();
        if !after_prompt.is_empty() {
            return Some(after_prompt.to_string());
        }
    }

    None
}

fn is_nudge_stuck(content: &str, nudge_text: &str) -> bool {
    let lines: Vec<&str> = content.lines().rev().take(5).collect();

    for line in lines {
        if let Some(pos) = line.find('❯') {
            let after_prompt = &line[pos + '❯'.len_utf8()..];
            let check_text = if nudge_text.len() > 20 {
                &nudge_text[..nudge_text.floor_char_boundary(20)]
            } else {
                nudge_text
            };
            if after_prompt.contains(check_text) {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
fn wait_for_empty_input(input_state: &SharedInputState, timeout: Duration) -> bool {
    let start = Instant::now();

    loop {
        let snapshot = snapshot_input_state(input_state);
        if snapshot.current_input.trim().is_empty() {
            return true;
        }

        if start.elapsed() >= timeout {
            info!(
                "Lead input not empty after {}s, nudging anyway",
                timeout.as_secs()
            );
            return false;
        }

        std::thread::sleep(NUDGE_POLL_INTERVAL);
    }
}

fn wait_for_nudge_safe_with_input_state(
    input_state: &SharedInputState,
    output_mirror: &SharedOutputMirror,
    last_nudge_text: Option<&str>,
    stable_duration: Duration,
    max_wait: Duration,
) -> bool {
    let start = Instant::now();

    loop {
        // Primary signal: check InputState (stdin side) for active typing
        let (current_input, last_change) = {
            match input_state.lock() {
                Ok(guard) => (guard.current_input.clone(), guard.last_change),
                Err(_) => (String::new(), Instant::now()),
            }
        };

        // If InputState shows empty input, safe to nudge immediately
        if current_input.trim().is_empty() {
            debug!("InputState empty, safe to nudge");
            return true;
        }

        // Check if the current input matches the last nudge (safe to overwrite)
        if let Some(last_nudge) = last_nudge_text {
            let check_len = last_nudge.floor_char_boundary(last_nudge.len().min(30));
            if check_len > 0 && current_input.starts_with(&last_nudge[..check_len]) {
                debug!("InputState contains last nudge text, safe to overwrite");
                return true;
            }
        }

        // If input has been stable (no keystrokes) for stable_duration, safe to append
        if last_change.elapsed() >= stable_duration {
            debug!(
                "InputState stable for {}s, safe to append",
                stable_duration.as_secs()
            );
            return true;
        }

        // Fallback: check OutputMirror (stdout side) as a secondary signal
        // This handles cases where InputState might not be tracking correctly
        let content = capture_output_text(output_mirror);
        let output_input = get_input_text(&content);

        if output_input.as_deref().is_none_or(|v| v.trim().is_empty())
            && current_input.trim().is_empty()
        {
            debug!("Both InputState and OutputMirror show no input, safe to nudge");
            return true;
        }

        // Max wait exceeded — nudge anyway to prevent indefinite blocking
        if start.elapsed() >= max_wait {
            info!(
                "Nudge wait timed out after {}s with active user input",
                max_wait.as_secs()
            );
            return false;
        }

        // Sleep for a short interval to avoid busy-waiting, but not so long that
        // we miss the stable_duration threshold. Use min(stable_duration / 4, NUDGE_POLL_INTERVAL).
        let poll_interval = stable_duration.div_f32(4.0).min(NUDGE_POLL_INTERVAL);
        std::thread::sleep(poll_interval);
    }
}

fn send_submit_with_retry(
    writer: &SharedWriter,
    output_mirror: &SharedOutputMirror,
    payload: &str,
) -> Result<(), String> {
    for attempt in 0..SUBMIT_RETRY_ATTEMPTS {
        if attempt > 0 {
            debug!(
                "Nudge verification: retrying Enter (attempt {})",
                attempt + 1
            );
            std::thread::sleep(SUBMIT_RETRY_DELAY);
        }

        write_submit_to_pty(writer)?;
        std::thread::sleep(SUBMIT_PROCESS_DELAY);

        let content = capture_output_text(output_mirror);
        if content.is_empty() || !is_nudge_stuck(&content, payload) {
            return Ok(());
        }
    }

    Err(format!(
        "Nudge submit failed after {} attempts - text may still be on input line",
        SUBMIT_RETRY_ATTEMPTS
    ))
}

fn deliver_payload(
    msg: &HeadedPollMessage,
    payload: &str,
    session: &str,
    on_message_cmd: Option<&str>,
) -> Result<(), String> {
    if let Some(cmd) = on_message_cmd {
        let status = Command::new("sh")
            .args(["-c", cmd])
            .env("MIDTOWN_HEADED_PAYLOAD", payload)
            .env("MIDTOWN_HEADED_SESSION", session)
            .env("MIDTOWN_HEADED_MESSAGE_ID", msg.id.to_string())
            .env("MIDTOWN_HEADED_KIND", &msg.kind)
            .env("MIDTOWN_HEADED_SUBMIT", if msg.submit { "1" } else { "0" })
            .status()
            .map_err(|e| format!("Failed running on-message command: {}", e))?;
        if status.success() {
            return Ok(());
        }
        return Err(format!(
            "on-message command exited with status {:?}",
            status.code()
        ));
    }

    // Default: write payload to wrapper stdout (dry-run transport).
    let mut out = std::io::stdout().lock();
    writeln!(out, "{}", payload).map_err(|e| format!("Failed writing payload to stdout: {}", e))?;
    out.flush()
        .map_err(|e| format!("Failed flushing stdout payload: {}", e))
}

pub fn handle(cmd: &HeadedWrapperCommand, client: &DaemonClient) -> Result<Response, String> {
    match cmd {
        HeadedWrapperCommand::Register {
            session,
            adapter_id,
            provider,
        } => {
            let value = client.headed_register(session, adapter_id, (*provider).into())?;
            Ok(Response::Json { value })
        }
        HeadedWrapperCommand::Poll {
            session,
            adapter_id,
            after_id,
            limit,
        } => {
            let value = client.headed_poll(session, adapter_id, *after_id, *limit)?;
            Ok(Response::Json { value })
        }
        HeadedWrapperCommand::Ack {
            session,
            adapter_id,
            msg_id,
        } => {
            let value = client.headed_ack(session, adapter_id, *msg_id)?;
            Ok(Response::Json { value })
        }
        HeadedWrapperCommand::Unregister {
            session,
            adapter_id,
        } => {
            let value = client.headed_unregister(session, adapter_id)?;
            Ok(Response::Json { value })
        }
        HeadedWrapperCommand::Run {
            session,
            provider,
            adapter_id,
            poll_interval_ms,
            batch_limit,
            once,
            on_message_cmd,
        } => {
            let provider: midtown::auth::AuthProvider = (*provider).into();
            let adapter_id = adapter_id
                .clone()
                .unwrap_or_else(|| default_adapter_id(session));
            let adapter = midtown::headed_adapter::adapter_for(provider);
            let mut last_seen_id = 0u64;
            let mut last_heartbeat = Instant::now();
            let heartbeat_every = Duration::from_secs(10);

            let reg = client.headed_register(session, &adapter_id, provider)?;
            if let Some(acked_id) = reg.get("acked_id").and_then(|v| v.as_u64()) {
                last_seen_id = acked_id;
            }

            loop {
                let polled =
                    client.headed_poll(session, &adapter_id, last_seen_id, *batch_limit)?;
                let parsed: HeadedPollResult = serde_json::from_value(polled)
                    .map_err(|e| format!("Invalid headed.poll response: {}", e))?;

                for msg in parsed.messages {
                    let payload =
                        apply_submit_key(adapter.format_system_message(&msg.text), msg.submit);
                    deliver_payload(&msg, &payload, session, on_message_cmd.as_deref())?;

                    client.headed_ack(session, &adapter_id, msg.id)?;
                    last_seen_id = msg.id;
                }

                if parsed.capture_output {
                    // RunShell has no PTY mirror — send empty so the waiter unblocks
                    let _ = client.headed_output(session, "(no PTY output available)");
                }

                if *once {
                    break;
                }

                if last_heartbeat.elapsed() >= heartbeat_every {
                    let _ = client.headed_heartbeat(session, &adapter_id);
                    last_heartbeat = Instant::now();
                }

                std::thread::sleep(Duration::from_millis((*poll_interval_ms).max(50)));
            }

            // Best-effort unregister on clean exit.
            let _ = client.headed_unregister(session, &adapter_id);

            Ok(Response::message(format!(
                "headed wrapper exited (session={}, adapter_id={})",
                session, adapter_id
            )))
        }
        HeadedWrapperCommand::RunAgent {
            session,
            provider,
            adapter_id,
            poll_interval_ms,
            batch_limit,
            cwd,
            command,
        } => {
            let provider: midtown::auth::AuthProvider = (*provider).into();
            let adapter_id = adapter_id
                .clone()
                .unwrap_or_else(|| default_adapter_id(session));

            let pty_system = portable_pty::native_pty_system();
            let (cols, rows) = crossterm::terminal::size().unwrap_or((120, 40));
            let pty_pair = pty_system
                .openpty(portable_pty::PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|e| format!("Failed to create PTY: {}", e))?;

            let mut cmd = portable_pty::CommandBuilder::new(&command[0]);
            for arg in command.iter().skip(1) {
                cmd.arg(arg);
            }
            if let Some(dir) = cwd {
                cmd.cwd(dir);
            }
            cmd.env("MIDTOWN_HEADED_SESSION", session);
            cmd.env("MIDTOWN_HEADED_PROVIDER", provider.as_str());

            let mut child = pty_pair
                .slave
                .spawn_command(cmd)
                .map_err(|e| format!("Failed to spawn agent command: {}", e))?;

            let pty_reader = pty_pair
                .master
                .try_clone_reader()
                .map_err(|e| format!("Failed to clone PTY reader: {}", e))?;
            let pty_writer = pty_pair
                .master
                .take_writer()
                .map_err(|e| format!("Failed to open PTY writer: {}", e))?;
            let shared_writer: SharedWriter = Arc::new(Mutex::new(pty_writer));
            let input_state: SharedInputState = Arc::new(Mutex::new(InputState::default()));
            let output_mirror: SharedOutputMirror = Arc::new(Mutex::new(OutputMirror::default()));

            let raw_mode_enabled = crossterm::terminal::enable_raw_mode().is_ok();
            spawn_stdout_relay(pty_reader, Arc::clone(&output_mirror));
            spawn_stdin_relay(Arc::clone(&shared_writer), Arc::clone(&input_state));

            let adapter = midtown::headed_adapter::adapter_for(provider);
            let mut last_seen_id = 0u64;
            let mut last_heartbeat = Instant::now();
            let heartbeat_every = Duration::from_secs(10);
            let mut last_nudge_text: Option<String> = None;

            let reg = client.headed_register(session, &adapter_id, provider)?;
            if let Some(acked_id) = reg.get("acked_id").and_then(|v| v.as_u64()) {
                last_seen_id = acked_id;
            }

            let mut run_result: Result<(), String> = Ok(());
            loop {
                match child
                    .try_wait()
                    .map_err(|e| format!("Failed to poll agent process status: {}", e))
                {
                    Ok(Some(_status)) => break,
                    Ok(None) => {}
                    Err(e) => {
                        run_result = Err(e);
                        break;
                    }
                }

                // Check for terminal resize events
                if crossterm::event::poll(Duration::from_millis(0)).unwrap_or(false)
                    && let Ok(crossterm::event::Event::Resize(new_cols, new_rows)) =
                        crossterm::event::read()
                {
                    let new_size = portable_pty::PtySize {
                        rows: new_rows,
                        cols: new_cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    };
                    if let Err(e) = pty_pair.master.resize(new_size) {
                        debug!("Failed to resize PTY: {}", e);
                    }
                }

                // Poll daemon for nudges. Tolerate transient connection failures
                // (e.g., daemon restart) — just skip the poll cycle and retry next
                // iteration. The interactive session should survive daemon restarts.
                let parsed: HeadedPollResult =
                    match client.headed_poll(session, &adapter_id, last_seen_id, *batch_limit) {
                        Ok(value) => match serde_json::from_value(value) {
                            Ok(parsed) => parsed,
                            Err(_) => {
                                std::thread::sleep(Duration::from_secs(1));
                                continue;
                            }
                        },
                        Err(_) => {
                            std::thread::sleep(Duration::from_secs(1));
                            continue;
                        }
                    };

                for msg in parsed.messages {
                    // For all sessions (lead and coworkers), check InputState (stdin side)
                    // as the primary signal to know when the user is typing. OutputMirror
                    // (stdout side) is used as a fallback. Without this, nudges get
                    // injected while the user is actively typing and interrupt their work.
                    let safe = wait_for_nudge_safe_with_input_state(
                        &input_state,
                        &output_mirror,
                        last_nudge_text.as_deref(),
                        COWORKER_INPUT_STABLE_DURATION,
                        COWORKER_INPUT_MAX_WAIT,
                    );
                    if !safe {
                        info!(
                            "Input still active after {}s, nudging anyway",
                            COWORKER_INPUT_MAX_WAIT.as_secs()
                        );
                    }

                    let payload =
                        trim_trailing_linebreaks(adapter.format_system_message(&msg.text));

                    if let Err(e) = write_payload_to_pty(&shared_writer, &payload) {
                        run_result = Err(e);
                        break;
                    }
                    // Give Claude Code's TUI time to process the text before
                    // sending Enter — without this, \r arrives in the same
                    // read buffer as the payload and is treated as a newline
                    // character in the input rather than a submit action.
                    if msg.submit {
                        std::thread::sleep(PAYLOAD_SETTLE_DELAY);
                    }
                    if msg.submit
                        && let Err(e) =
                            send_submit_with_retry(&shared_writer, &output_mirror, &payload)
                    {
                        run_result = Err(e);
                        break;
                    }

                    // Ack is best-effort — if daemon is temporarily unreachable
                    // (e.g., restarting), don't kill the interactive session.
                    // We still advance last_seen_id so the next poll skips
                    // already-delivered messages.
                    if let Err(e) = client.headed_ack(session, &adapter_id, msg.id) {
                        info!("headed_ack failed (will retry next cycle): {}", e);
                    }
                    last_seen_id = msg.id;
                    last_nudge_text = Some(payload);
                }
                if run_result.is_err() {
                    break;
                }

                // On-demand PTY capture: daemon requested the wrapper's screen
                if parsed.capture_output {
                    let captured = capture_output_text(&output_mirror);
                    let _ = client.headed_output(session, &captured);
                }

                if last_heartbeat.elapsed() >= heartbeat_every {
                    let _ = client.headed_heartbeat(session, &adapter_id);
                    last_heartbeat = Instant::now();
                }

                std::thread::sleep(Duration::from_millis((*poll_interval_ms).max(50)));
            }

            let _ = client.headed_unregister(session, &adapter_id);

            if raw_mode_enabled {
                let _ = crossterm::terminal::disable_raw_mode();
            }

            if let Err(e) = run_result {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }

            let status = child
                .wait()
                .map_err(|e| format!("Failed waiting for agent process: {}", e))?;
            if status.success() {
                Ok(Response::message(format!(
                    "headed run-agent exited cleanly (session={}, adapter_id={})",
                    session, adapter_id
                )))
            } else {
                Err(format!(
                    "headed run-agent child exited with status {:?}",
                    status
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::{
        InputState, OutputMirror, SharedInputState, SharedOutputMirror, apply_submit_key,
        get_input_text, is_nudge_stuck, trim_trailing_linebreaks, update_input_state,
        wait_for_empty_input, wait_for_nudge_safe_with_input_state,
    };

    #[test]
    fn submit_key_appends_carriage_return() {
        assert_eq!(apply_submit_key("hello".to_string(), true), "hello\r");
        assert_eq!(apply_submit_key("hello\n".to_string(), true), "hello\r");
        assert_eq!(apply_submit_key("hello\r\n".to_string(), true), "hello\r");
    }

    #[test]
    fn submit_key_noop_when_submit_false() {
        assert_eq!(apply_submit_key("hello".to_string(), false), "hello");
    }

    #[test]
    fn trim_trailing_linebreaks_only() {
        assert_eq!(trim_trailing_linebreaks("hello\r\n".to_string()), "hello");
        assert_eq!(trim_trailing_linebreaks("hello\n".to_string()), "hello");
        assert_eq!(trim_trailing_linebreaks("hello".to_string()), "hello");
    }

    #[test]
    fn input_state_tracks_typing_and_clears_on_enter() {
        let state: SharedInputState = Arc::new(Mutex::new(InputState::default()));
        update_input_state(&state, b"hello");
        let snap = super::snapshot_input_state(&state);
        assert_eq!(snap.current_input, "hello");

        update_input_state(&state, b"\x7f");
        let snap = super::snapshot_input_state(&state);
        assert_eq!(snap.current_input, "hell");

        update_input_state(&state, b"\r");
        let snap = super::snapshot_input_state(&state);
        assert!(snap.current_input.is_empty());
    }

    #[test]
    fn wait_for_empty_input_returns_quickly_when_empty() {
        let state: SharedInputState = Arc::new(Mutex::new(InputState::default()));
        assert!(wait_for_empty_input(&state, Duration::from_millis(10)));
    }

    #[test]
    fn get_input_text_prefers_most_recent_prompt() {
        let content = "older\n❯ first\n\nnew\n❯ second";
        assert_eq!(get_input_text(content).as_deref(), Some("second"));
    }

    #[test]
    fn nudge_stuck_detection_matches_prompt_line() {
        let content = "something\n❯ github said: check ci";
        assert!(is_nudge_stuck(content, "github said: check ci on pr #10"));
        assert!(!is_nudge_stuck(content, "totally different"));
    }

    #[test]
    fn wait_for_nudge_safe_overwrites_last_nudge_immediately() {
        let input_state: SharedInputState = Arc::new(Mutex::new(InputState::default()));
        let mirror: SharedOutputMirror = Arc::new(Mutex::new(OutputMirror::default()));

        // InputState contains the last nudge text
        update_input_state(&input_state, b"github said: check ci");

        {
            let mut guard = mirror.lock().expect("mirror lock");
            guard.ingest("❯ github said: check ci\n".as_bytes());
        }

        let safe = wait_for_nudge_safe_with_input_state(
            &input_state,
            &mirror,
            Some("github said: check ci"),
            Duration::from_secs(20),
            Duration::from_secs(1),
        );
        assert!(safe);
    }

    #[test]
    fn wait_for_nudge_safe_respects_active_input_state() {
        let input_state: SharedInputState = Arc::new(Mutex::new(InputState::default()));
        let mirror: SharedOutputMirror = Arc::new(Mutex::new(OutputMirror::default()));

        // User is actively typing (recent keystroke, non-empty input)
        update_input_state(&input_state, b"hello");

        {
            let mut guard = mirror.lock().expect("mirror lock");
            // OutputMirror might not have caught up yet — empty or showing old prompt
            guard.ingest("❯ \n".as_bytes());
        }

        // Simulate continued typing in a background thread
        let input_state_clone = Arc::clone(&input_state);
        let typing_thread = std::thread::spawn(move || {
            // Keep typing every 50ms for 400ms (total)
            for _ in 0..8 {
                std::thread::sleep(Duration::from_millis(50));
                update_input_state(&input_state_clone, b"x");
            }
        });

        // Should wait because InputState shows active typing
        let safe = wait_for_nudge_safe_with_input_state(
            &input_state,
            &mirror,
            None,
            Duration::from_millis(150),
            Duration::from_millis(500),
        );

        typing_thread.join().unwrap();

        // Should time out waiting for input to stabilize
        assert!(!safe);
    }

    #[test]
    fn wait_for_nudge_safe_allows_nudge_when_input_empty() {
        let input_state: SharedInputState = Arc::new(Mutex::new(InputState::default()));
        let mirror: SharedOutputMirror = Arc::new(Mutex::new(OutputMirror::default()));

        {
            let mut guard = mirror.lock().expect("mirror lock");
            guard.ingest("❯ \n".as_bytes());
        }

        // Input state is empty — safe to nudge immediately
        let safe = wait_for_nudge_safe_with_input_state(
            &input_state,
            &mirror,
            None,
            Duration::from_secs(20),
            Duration::from_secs(1),
        );
        assert!(safe);
    }
}
