use std::{path::Path, thread, time::Duration};

use serde_json::Value;

use crate::{
    channel::Channel,
    matrix_bridge::{client::MatrixClient, state::MatrixBridgeState},
    message::Message,
};

const OUTBOUND_SYNC_INTERVAL: Duration = Duration::from_secs(1);

pub fn run_outbound_sync(
    client: &MatrixClient,
    project: &str,
    state_path: &Path,
) -> Result<(), String> {
    if project.trim().is_empty() {
        return Err("project cannot be empty".to_string());
    }

    let project_dir = crate::paths::projects_dir_for_repo(project);
    let mut state = MatrixBridgeState::load(state_path)?;

    loop {
        let channel_infos = Channel::list(&project_dir, false, Some(project))
            .map_err(|e| format!("failed to list channels for {project}: {e}"))?;
        for channel_info in channel_infos {
            if let Err(e) = sync_channel(client, &project_dir, &channel_info.name, &mut state) {
                eprintln!(
                    "matrix outbound sync failed for channel {}: {e}",
                    channel_info.name
                );
            }
        }

        if let Err(e) = state.save(state_path) {
            eprintln!("failed to persist matrix bridge state: {e}");
        }

        thread::sleep(OUTBOUND_SYNC_INTERVAL);
    }
}

fn sync_channel(
    client: &MatrixClient,
    project_dir: &Path,
    channel_name: &str,
    state: &mut MatrixBridgeState,
) -> Result<(), String> {
    let channel = Channel::new(project_dir, channel_name)
        .map_err(|e| format!("failed to open channel '{}': {e}", channel_name))?;
    let messages = channel
        .read_all()
        .map_err(|e| format!("failed to read channel '{}': {e}", channel_name))?;
    if messages.is_empty() {
        return Ok(());
    }

    let room_id = room_for_channel(client, channel_name, state)?;
    let unsynced = unsynced_messages(
        &messages,
        state.last_synced.get(channel_name).map(String::as_str),
    );

    if unsynced.is_empty() && !state.last_synced.contains_key(channel_name) {
        if let Some(last_message) = messages.last() {
            state
                .last_synced
                .insert(channel_name.to_string(), last_message.id.clone());
        }
        return Ok(());
    }

    for message in unsynced {
        let sender_user_id = user_for_message(client, &message.from, state)?;
        let body = format_matrix_body(&message.content);
        let thread_parent_event_id =
            message
                .thread_parent_id
                .as_ref()
                .and_then(|thread_parent_id| {
                    state
                        .matrix_events
                        .get(thread_parent_id)
                        .map(String::as_str)
                });

        let event_id = client.send_message(
            room_id.as_str(),
            sender_user_id.as_str(),
            body.as_str(),
            thread_parent_event_id,
        )?;
        state.matrix_events.insert(message.id.clone(), event_id);
        state
            .last_synced
            .insert(channel_name.to_string(), message.id.clone());
    }

    Ok(())
}

fn room_for_channel(
    client: &MatrixClient,
    channel_name: &str,
    state: &mut MatrixBridgeState,
) -> Result<String, String> {
    if let Some(room_id) = state.rooms.get(channel_name) {
        return Ok(room_id.clone());
    }

    let room_id = client.ensure_room_exists(channel_name)?;
    state
        .rooms
        .insert(channel_name.to_string(), room_id.clone());
    Ok(room_id)
}

pub fn room_for_channel_name<'a>(state: &'a MatrixBridgeState, room_id: &str) -> Option<&'a str> {
    state
        .rooms
        .iter()
        .find_map(|(channel, id)| (id == room_id).then_some(channel.as_str()))
}

fn user_for_message(
    client: &MatrixClient,
    username: &str,
    state: &mut MatrixBridgeState,
) -> Result<String, String> {
    if let Some(user_id) = state.users.get(username) {
        return Ok(user_id.clone());
    }

    client.ensure_virtual_user_exists(username)?;
    let user_id = client.user_id(username);
    state.users.insert(username.to_string(), user_id.clone());
    Ok(user_id)
}

fn unsynced_messages(messages: &[Message], last_synced: Option<&str>) -> Vec<Message> {
    if let Some(last_synced) = last_synced {
        let mut found_anchor = false;
        let mut unsynced = Vec::new();
        let mut after_anchor = false;

        for message in messages {
            if after_anchor {
                unsynced.push(message.clone());
                continue;
            }
            if message.id == last_synced {
                after_anchor = true;
                found_anchor = true;
            }
        }

        if found_anchor {
            return unsynced;
        }
    }

    messages.to_vec()
}

fn format_matrix_body(content: &str) -> String {
    let trimmed = content.trim();
    if is_fenced_block(trimmed) {
        return content.to_string();
    }

    if let Some(wrapped) = wrapped_tool_payload(trimmed) {
        return wrapped;
    }

    content.to_string()
}

fn is_fenced_block(content: &str) -> bool {
    content.starts_with("```")
}

fn wrapped_tool_payload(content: &str) -> Option<String> {
    let payload: Value = serde_json::from_str(content).ok()?;
    if !is_tool_payload(&payload) {
        return None;
    }

    serde_json::to_string_pretty(&payload)
        .ok()
        .map(|pretty| format!("```json\n{pretty}\n```"))
}

fn is_tool_payload(content: &Value) -> bool {
    match content {
        Value::Object(object) => object
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| t == "tool_use" || t == "tool_result"),
        Value::Array(items) => items.iter().all(is_tool_payload),
        _ => false,
    }
}
