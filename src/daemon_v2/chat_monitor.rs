//! Background chat monitor — watches channel JSONL files for new messages
//! and routes @mentions through the decision system.
//!
//! Section 15 (Critical): "Background chat monitor (tail loop on channel
//! JSONL for ambient mention routing)"

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::{Mutex, watch};

use crate::daemon_v2::decisions::Command;
use crate::daemon_v2::projections::Projections;
use crate::message::Message;

/// Senders whose messages should not be routed (system messages, daemon itself).
const SKIP_SENDERS: &[&str] = &["midtown", "system", "github"];

/// Start the chat monitor background task.
///
/// Watches the default channel's JSONL file for new messages. When a message
/// from a non-system sender arrives, routes it through `route_message` and
/// sends any resulting commands (nudges) to the daemon event loop.
pub async fn chat_monitor_loop(
    channels_dir: PathBuf,
    default_channel: String,
    projections: Arc<Mutex<Projections>>,
    command_tx: tokio::sync::mpsc::Sender<Command>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let channel_file = channels_dir
        .join("channels")
        .join(&default_channel)
        .join("history")
        .join("current.jsonl");

    // Wait for the channel file to exist
    for _ in 0..30 {
        if channel_file.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    if !channel_file.exists() {
        tracing::warn!(
            path = %channel_file.display(),
            "chat monitor: channel file not found, disabling"
        );
        return;
    }

    // Start tailing from the end (0 = no initial lines)
    let mut tailer = match tailf::tailf(&channel_file, Some(0)) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(%e, "chat monitor: failed to start tailf");
            return;
        }
    };

    tracing::info!(
        channel = %default_channel,
        path = %channel_file.display(),
        "chat monitor started"
    );

    loop {
        tokio::select! {
            Some(result) = async { Some(tailer.next().await) } => {
                match result {
                    Ok(Some(bytes)) => {
                        let line = match String::from_utf8(bytes) {
                            Ok(s) => s,
                            Err(_) => continue,
                        };
                        handle_new_message(
                            &line,
                            &default_channel,
                            &projections,
                            &command_tx,
                        ).await;
                    }
                    Ok(None) => {
                        // File rotated or EOF — wait and retry
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                    Err(e) => {
                        tracing::warn!(%e, "chat monitor: tailf error");
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    tracing::info!("chat monitor shutting down");
                    break;
                }
            }
        }
    }
}

async fn handle_new_message(
    line: &str,
    channel: &str,
    projections: &Arc<Mutex<Projections>>,
    command_tx: &tokio::sync::mpsc::Sender<Command>,
) {
    let msg: Message = match serde_json::from_str(line) {
        Ok(m) => m,
        Err(_) => return,
    };

    // Skip system/daemon messages
    if SKIP_SENDERS.contains(&msg.from.as_str()) {
        return;
    }

    // Skip messages that were posted by the daemon's channel.post RPC
    // (those already get routed via route_message in the RPC handler).
    // We only want to catch messages written directly by agents via
    // PostToolUse hooks.
    //
    // Heuristic: if the sender is "user", it came through the RPC/WS path
    // and was already routed. Agent messages have the agent's name as sender.
    if msg.from == "user" {
        return;
    }

    // Check for @mentions or other routing triggers
    let content = &msg.content;
    if !content.contains('@') && !content.contains('!') {
        return;
    }

    let thread_id = msg.thread_parent_id.as_deref();

    // Route through the decision function
    let commands = {
        let proj = projections.lock().await;
        crate::daemon_v2::decisions::chat::route_message(
            &proj, channel, &msg.from, content, thread_id,
        )
    };

    // Send commands to daemon
    for cmd in commands {
        if let Err(e) = command_tx.send(cmd).await {
            tracing::warn!(%e, "chat monitor: failed to send command");
            break;
        }
    }
}

/// Start chat monitors for all known channels.
pub async fn start_monitors(
    channels_dir: &Path,
    default_channel: &str,
    projections: Arc<Mutex<Projections>>,
    command_tx: tokio::sync::mpsc::Sender<Command>,
    shutdown_rx: watch::Receiver<bool>,
) {
    // Monitor the default channel
    let dir = channels_dir.to_path_buf();
    let channel = default_channel.to_string();
    let proj = projections.clone();
    let tx = command_tx.clone();
    let rx = shutdown_rx.clone();

    tokio::spawn(async move {
        chat_monitor_loop(dir, channel, proj, tx, rx).await;
    });

    // Also monitor any existing channels
    let channels_root = channels_dir.join("channels");
    if let Ok(entries) = std::fs::read_dir(&channels_root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == default_channel || name.ends_with(".archived") || name.starts_with('.') {
                continue;
            }

            let dir = channels_dir.to_path_buf();
            let proj = projections.clone();
            let tx = command_tx.clone();
            let rx = shutdown_rx.clone();

            tokio::spawn(async move {
                chat_monitor_loop(dir, name, proj, tx, rx).await;
            });
        }
    }
}
