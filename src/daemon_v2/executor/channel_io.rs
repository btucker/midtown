use std::path::Path;

use serde_json::Value;

use crate::channel::{Channel, ChannelInfo};
use crate::message::{Message, MessageType};

/// Post a user message to a channel.
/// Post a user message to a channel. Returns the message ID.
pub fn post_message(
    channels_dir: &Path,
    channel: &str,
    sender: &str,
    content: &str,
    thread_id: Option<&str>,
) -> Result<String, String> {
    let ch = Channel::new(channels_dir, channel).map_err(|e| e.to_string())?;
    let mut msg = Message::for_channel(channel, sender, content, MessageType::Text);
    if let Some(tid) = thread_id {
        msg.thread_parent_id = Some(tid.to_string());
    }
    let id = msg.id.clone();
    ch.send(&msg).map_err(|e| e.to_string())?;
    Ok(id)
}

/// Post an auto-output message (from agent drain loop). Sets `auto_output: true`
/// so the web UI can filter these when "show full lead output" is off.
/// Optionally includes tool data blocks.
pub fn post_auto_output(
    channels_dir: &Path,
    channel: &str,
    sender: &str,
    content: &str,
    tool_data: Option<Vec<crate::message::ToolBlock>>,
    thread_id: Option<&str>,
) -> Result<String, String> {
    let ch = Channel::new(channels_dir, channel).map_err(|e| e.to_string())?;
    let mut msg = Message::for_channel(channel, sender, content, MessageType::Text);
    msg.auto_output = true;
    msg.tool_data = tool_data;
    if let Some(tid) = thread_id {
        msg.thread_parent_id = Some(tid.to_string());
    }
    let id = msg.id.clone();
    ch.send(&msg).map_err(|e| e.to_string())?;
    Ok(id)
}

/// Post a system message to a channel.
pub fn post_system_message(
    channels_dir: &Path,
    channel: &str,
    content: &str,
) -> Result<(), String> {
    let ch = Channel::new(channels_dir, channel).map_err(|e| e.to_string())?;
    let msg = Message::system(content);
    ch.send(&msg).map_err(|e| e.to_string())
}

/// Read messages from a channel, optionally limited to the last N.
/// Per spec 5.3: thread replies are excluded unless reading a specific thread.
pub fn read_messages(
    channels_dir: &Path,
    channel: &str,
    limit: Option<usize>,
    before: Option<&str>,
) -> Result<Vec<Value>, String> {
    read_messages_filtered(channels_dir, channel, limit, None, before)
}

/// Read messages from a specific thread.
/// Per spec 5.3: returns the parent message and all replies with that thread_parent_id.
pub fn read_thread_messages(
    channels_dir: &Path,
    channel: &str,
    thread_parent_id: &str,
    limit: Option<usize>,
) -> Result<Vec<Value>, String> {
    read_messages_filtered(channels_dir, channel, limit, Some(thread_parent_id), None)
}

/// Internal: read messages with optional thread filtering and pagination.
/// `before`: if set, only return messages with timestamp < this value (ISO string).
fn read_messages_filtered(
    channels_dir: &Path,
    channel: &str,
    limit: Option<usize>,
    thread_parent_id: Option<&str>,
    before: Option<&str>,
) -> Result<Vec<Value>, String> {
    let ch = Channel::new(channels_dir, channel).map_err(|e| e.to_string())?;
    let all_messages = ch.read_all().map_err(|e| e.to_string())?;

    let messages: Vec<_> = if let Some(tid) = thread_parent_id {
        // Thread read: include the parent message + all replies
        all_messages
            .iter()
            .filter(|m| m.id == tid || m.thread_parent_id.as_deref() == Some(tid))
            .collect()
    } else {
        // Default: exclude thread replies
        all_messages
            .iter()
            .filter(|m| m.thread_parent_id.is_none())
            .collect()
    };

    // Apply `before` cursor for pagination
    let messages: Vec<_> = if let Some(before_ts) = before {
        if let Ok(cutoff) = chrono::DateTime::parse_from_rfc3339(before_ts) {
            let cutoff_utc = cutoff.with_timezone(&chrono::Utc);
            messages
                .into_iter()
                .filter(|m| m.timestamp < cutoff_utc)
                .collect()
        } else {
            messages
        }
    } else {
        messages
    };

    let msgs: Vec<Value> = if let Some(n) = limit {
        messages
            .iter()
            .rev()
            .take(n)
            .rev()
            .map(|m| m.to_json())
            .collect()
    } else {
        messages.iter().map(|m| m.to_json()).collect()
    };
    Ok(msgs)
}

/// List all channels in the channels directory.
pub fn list_channels(channels_dir: &Path) -> Result<Vec<ChannelInfo>, String> {
    Channel::list(channels_dir, false, None).map_err(|e| e.to_string())
}

#[path = "channel_io_tests.rs"]
#[cfg(test)]
mod tests;
