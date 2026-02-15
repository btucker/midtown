//! Headed wrapper CLI.
//!
//! Provides an adapter-neutral intercom loop for headed sessions:
//! register -> poll -> deliver -> ack.

use std::io::Write;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::cli::Response;
use crate::client::DaemonClient;

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
}

fn default_adapter_id(session: &str) -> String {
    format!("midtown-wrapper-{}-{}", session, std::process::id())
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
                    let mut payload = adapter.format_system_message(&msg.text);
                    if msg.submit && !payload.ends_with('\n') {
                        payload.push('\n');
                    }
                    deliver_payload(&msg, &payload, session, on_message_cmd.as_deref())?;

                    client.headed_ack(session, &adapter_id, msg.id)?;
                    last_seen_id = msg.id;
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
    }
}
