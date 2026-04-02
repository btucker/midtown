use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

use super::WebState;
use crate::daemon_v2::rpc;

pub async fn health() -> &'static str {
    "ok"
}

pub async fn status(State(state): State<Arc<WebState>>) -> Json<Value> {
    let proj = state.projections.lock().await;
    let (response, _, _) = rpc::dispatch_request(
        json!({"jsonrpc": "2.0", "method": "status", "id": 1}),
        &proj,
        &state.channels_dir,
    );
    let mut result = response.get("result").cloned().unwrap_or(json!(null));
    if let Some(obj) = result.as_object_mut() {
        obj.insert("repo_name".to_string(), json!(state.repo_name));
        obj.insert("repo_full_name".to_string(), json!(state.repo_full_name));
    }
    Json(result)
}

#[derive(Deserialize)]
pub struct ChannelHistoryParams {
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    limit: Option<u64>,
    #[serde(default)]
    last: Option<u64>,
    #[serde(default)]
    thread_parent_id: Option<String>,
    /// Pagination cursor: only return messages with timestamp before this ISO string.
    #[serde(default)]
    before: Option<String>,
}

pub async fn channel_history(
    State(state): State<Arc<WebState>>,
    Query(params): Query<ChannelHistoryParams>,
) -> Json<Value> {
    let channel = params.channel.as_deref().unwrap_or("midtown");
    let limit = params.limit.or(params.last).or(Some(100));

    // Build RPC params — include thread_parent_id and before if specified
    let mut rpc_params = json!({"channel": channel, "limit": limit});
    if let Some(ref parent_id) = params.thread_parent_id {
        rpc_params["thread_parent_id"] = json!(parent_id);
    }
    if let Some(ref before) = params.before {
        rpc_params["before"] = json!(before);
    }

    // Spec 8.1: channel.read only does filesystem I/O — snapshot projections briefly
    let (response, _, _) = {
        let proj = state.projections.lock().await;
        rpc::dispatch_request(
            json!({
                "jsonrpc": "2.0",
                "method": "channel.read",
                "params": rpc_params,
                "id": 1
            }),
            &proj,
            &state.channels_dir,
        )
    };
    let messages = response.get("result").cloned().unwrap_or(json!([]));

    // Transform "message" field to "content" for web UI compatibility
    let messages = match messages {
        Value::Array(arr) => Value::Array(
            arr.into_iter()
                .map(|mut msg| {
                    if let Value::Object(ref mut map) = msg
                        && let Some(content) = map.remove("message")
                    {
                        map.insert("content".to_string(), content);
                    }
                    msg
                })
                .collect(),
        ),
        other => other,
    };
    Json(messages)
}

/// POST /api/channels/history — post a message to a channel.
/// Routes through channel.post RPC to get message routing (nudges, etc.).
pub async fn channel_post(
    State(state): State<Arc<WebState>>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let channel = body
        .get("channel")
        .and_then(|v| v.as_str())
        .unwrap_or("midtown");
    let sender = body
        .get("sender")
        .or_else(|| body.get("from"))
        .and_then(|v| v.as_str())
        .unwrap_or("user");
    let content = body
        .get("content")
        .or_else(|| body.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let thread_id = body.get("thread_parent_id").and_then(|v| v.as_str());

    if content.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "content is required"})),
        );
    }

    let rpc_request = json!({
        "jsonrpc": "2.0",
        "method": "channel.post",
        "params": {
            "channel": channel,
            "sender": sender,
            "content": content,
            "thread_id": thread_id,
        },
        "id": 1,
    });

    let (response, events, commands) = {
        let proj = state.projections.lock().await;
        rpc::dispatch_request(rpc_request, &proj, &state.channels_dir)
    };

    // Apply events to projections + broadcast + persist via daemon
    if !events.is_empty() {
        {
            let mut proj = state.projections.lock().await;
            for event in &events {
                proj.apply(event);
                let _ = state.event_tx.send(event.clone());
            }
        }
        let persist = crate::daemon_v2::decisions::Command::PersistEvents(events);
        let _ = state.command_tx.send(persist).await;
    }

    // Send routing commands to daemon for execution
    for cmd in commands {
        if let Err(e) = state.command_tx.send(cmd).await {
            tracing::warn!(%e, "failed to send channel.post command");
        }
    }

    // Check for RPC error before returning success
    if let Some(error) = response.get("error") {
        let message = error
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown error");
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": message})),
        );
    }

    let msg_id = response
        .get("result")
        .and_then(|r| r.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    (StatusCode::OK, Json(json!({"ok": true, "id": msg_id})))
}

#[derive(Deserialize)]
pub struct ChannelListParams {
    #[serde(default)]
    include_archived: Option<bool>,
}

pub async fn channel_list(
    State(state): State<Arc<WebState>>,
    Query(params): Query<ChannelListParams>,
) -> Json<Value> {
    let include_archived = params.include_archived.unwrap_or(false);
    let channels = crate::channel::Channel::list(&state.channels_dir, include_archived, None)
        .unwrap_or_default();
    let list: Vec<Value> = channels
        .iter()
        .map(|c| {
            json!({
                "name": c.name,
                "is_archived": c.is_archived,
                "is_dm": c.is_dm,
            })
        })
        .collect();
    Json(json!({ "channels": list }))
}

#[derive(Deserialize)]
pub struct CreateChannelBody {
    name: String,
}

pub async fn channel_create(
    State(state): State<Arc<WebState>>,
    Json(body): Json<CreateChannelBody>,
) -> (StatusCode, Json<Value>) {
    let rpc_request = json!({
        "jsonrpc": "2.0",
        "method": "channel.create",
        "params": { "name": body.name },
        "id": 1,
    });

    let (response, events, _commands) = {
        let proj = state.projections.lock().await;
        rpc::dispatch_request(rpc_request, &proj, &state.channels_dir)
    };

    if response.get("error").is_some() {
        let msg = response
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|v| v.as_str())
            .unwrap_or("failed to create channel");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": msg})),
        );
    }

    // Apply events to projections + broadcast via WebSocket
    if !events.is_empty() {
        {
            let mut proj = state.projections.lock().await;
            for event in &events {
                proj.apply(event);
                let _ = state.event_tx.send(event.clone());
            }
        }
        let persist = crate::daemon_v2::decisions::Command::PersistEvents(events);
        let _ = state.command_tx.send(persist).await;
    }

    (
        StatusCode::CREATED,
        Json(json!({"ok": true, "name": body.name})),
    )
}

// Stub routes for endpoints the web UI calls but v2 doesn't fully implement yet.
// Return empty/default data instead of 404 so the UI doesn't break.

pub async fn read_state(State(state): State<Arc<WebState>>) -> Json<Value> {
    let path = read_state_path(&state.channels_dir);
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            let data: Value = serde_json::from_str(&contents).unwrap_or(json!({}));
            Json(data)
        }
        Err(_) => Json(json!({})),
    }
}

/// Read state file lives next to the project's channels directory.
fn read_state_path(channels_dir: &std::path::Path) -> std::path::PathBuf {
    channels_dir.join("read-state.json")
}

fn load_read_state(channels_dir: &std::path::Path) -> Value {
    let path = read_state_path(channels_dir);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(json!({"channels": {}, "threads": {}}))
}

fn save_read_state(channels_dir: &std::path::Path, data: &Value) {
    let path = read_state_path(channels_dir);
    if let Ok(json) = serde_json::to_string(data) {
        let _ = std::fs::write(&path, json);
    }
}

pub async fn usage() -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    // Collect active provider/profile combinations
    let mut profiles = Vec::new();
    for provider in &[
        crate::auth::AuthProvider::Claude,
        crate::auth::AuthProvider::Codex,
    ] {
        let profile_dir = crate::auth::current_profile_dir_for(*provider);
        if profile_dir.exists() {
            let profile = crate::auth::current_profile_for(*provider);
            profiles.push((*provider, profile));
        }
    }

    if profiles.is_empty() {
        return StatusCode::NO_CONTENT.into_response();
    }

    // Fetch real usage data (blocking — hits Anthropic OAuth API)
    let data = match tokio::task::spawn_blocking(move || crate::usage::fetch_multi_usage(&profiles))
        .await
    {
        Ok(data) if !data.is_empty() => data,
        Ok(_) => return StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::warn!(%e, "usage fetch failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Build response matching v1 format
    let usage: Vec<Value> = data
        .iter()
        .map(|d| {
            json!({
                "provider": d.provider.as_str(),
                "profile": d.profile_name,
                "account_email": d.account_email,
                "session_util": d.session_util,
                "session_resets": d.session_resets,
                "week_util": d.week_util,
                "week_resets": d.week_resets,
            })
        })
        .collect();

    Json(json!({ "usage": usage })).into_response()
}

pub async fn questions() -> Json<Value> {
    Json(json!([]))
}

pub async fn workflow(Query(params): Query<HashMap<String, String>>) -> Json<Value> {
    let _channel = params.get("channel").cloned().unwrap_or_default();
    // Workflow system is not yet ported to v2 — return empty state
    Json(json!({
        "assigned": false,
        "workflow": null,
        "lead_driven": false,
        "state": {}
    }))
}

/// GET /api/channels/{channel}/workflow — per-channel workflow state
pub async fn channel_workflow_get(
    State(state): State<Arc<WebState>>,
    axum::extract::Path(channel): axum::extract::Path<String>,
) -> Json<Value> {
    let proj = state.projections.lock().await;
    let lead_driven = proj.channels.is_lead_driven(&channel);
    Json(json!({
        "assigned": false,
        "workflow": null,
        "lead_driven": lead_driven,
        "state": {}
    }))
}

/// PUT /api/channels/{channel}/workflow — update workflow settings (lead_driven toggle)
pub async fn channel_workflow_put(
    State(state): State<Arc<WebState>>,
    axum::extract::Path(channel): axum::extract::Path<String>,
    Json(body): Json<Value>,
) -> Json<Value> {
    // Delegate to the same settings update path
    let mut params = json!({"channel": channel});
    if let Some(v) = body.get("lead_driven") {
        params["lead_driven"] = v.clone();
    }
    if let Some(v) = body.get("workflow") {
        params["workflow"] = v.clone();
    }
    let proj = state.projections.lock().await;
    let (_response, events, _) = rpc::dispatch_request(
        json!({"jsonrpc": "2.0", "method": "channel.update", "params": params, "id": 1}),
        &proj,
        &state.channels_dir,
    );
    drop(proj);
    if !events.is_empty() {
        {
            let mut proj = state.projections.lock().await;
            for event in &events {
                proj.apply(event);
                let _ = state.event_tx.send(event.clone());
            }
        }
        let persist = crate::daemon_v2::decisions::Command::PersistEvents(events);
        let _ = state.command_tx.send(persist).await;
    }
    Json(json!({"ok": true}))
}

pub async fn auth_profiles(Query(params): Query<AuthProfilesParams>) -> Json<Value> {
    let provider_str = params.provider.as_deref().unwrap_or("claude");
    let auth_provider = match provider_str {
        "codex" => crate::auth::AuthProvider::Codex,
        _ => crate::auth::AuthProvider::Claude,
    };

    let current = crate::auth::current_profile_for(auth_provider);
    let profiles = crate::auth::list_profiles_for(auth_provider).unwrap_or_default();

    let result: Vec<Value> = if profiles.is_empty() {
        vec![json!({
            "name": "default",
            "is_current": true,
            "has_credentials": true,
            "provider": provider_str,
        })]
    } else {
        profiles
            .iter()
            .map(|name| {
                json!({
                    "name": name,
                    "is_current": name == &current,
                    "has_credentials": true,
                    "provider": provider_str,
                })
            })
            .collect()
    };

    Json(json!(result))
}

#[derive(Deserialize)]
pub struct AuthProfilesParams {
    #[serde(default)]
    provider: Option<String>,
}

#[derive(Deserialize)]
pub struct SearchParams {
    q: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
}

fn default_search_limit() -> usize {
    50
}

pub async fn search(
    State(state): State<Arc<WebState>>,
    Query(params): Query<SearchParams>,
) -> Json<Value> {
    // Search across all channels for messages containing the query
    let channels = crate::daemon_v2::executor::channel_io::list_channels(&state.channels_dir)
        .unwrap_or_default();

    let mut results = Vec::new();
    let query_lower = params.q.to_lowercase();

    for ch in &channels {
        if results.len() >= params.limit {
            break;
        }
        if let Ok(messages) = crate::daemon_v2::executor::channel_io::read_messages(
            &state.channels_dir,
            &ch.name,
            Some(200),
            None,
        ) {
            for msg in &messages {
                if results.len() >= params.limit {
                    break;
                }
                let content = msg
                    .get("message")
                    .or_else(|| msg.get("content"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if content.to_lowercase().contains(&query_lower) {
                    let mut result = msg.clone();
                    // Ensure content field exists
                    if let Value::Object(ref mut map) = result {
                        if let Some(c) = map.remove("message") {
                            map.insert("content".to_string(), c);
                        }
                        map.insert("channel".to_string(), json!(ch.name));
                    }
                    results.push(result);
                }
            }
        }
    }

    Json(json!({
        "results": results,
        "query": params.q,
        "total": results.len(),
    }))
}

pub async fn channel_settings_get(
    State(state): State<Arc<WebState>>,
    axum::extract::Path(channel): axum::extract::Path<String>,
) -> Json<Value> {
    let proj = state.projections.lock().await;
    let settings = proj
        .channels
        .channels
        .get(&channel)
        .map(|meta| {
            json!({
                "show_full_lead_output": meta.settings.show_full_lead_output,
                "lead_driven": meta.settings.lead_driven,
                "directory": meta.settings.directory,
            })
        })
        .unwrap_or_else(|| json!({"show_full_lead_output": true, "lead_driven": false}));
    Json(settings)
}

pub async fn channel_settings_put(
    State(state): State<Arc<WebState>>,
    axum::extract::Path(channel): axum::extract::Path<String>,
    Json(body): Json<Value>,
) -> Json<Value> {
    // Apply settings via RPC channel.update
    let mut params = json!({"channel": channel});
    if let Some(v) = body.get("show_full_lead_output") {
        params["show_full_lead_output"] = v.clone();
    }
    if let Some(v) = body.get("lead_driven") {
        params["lead_driven"] = v.clone();
    }
    if let Some(v) = body.get("directory") {
        params["directory"] = v.clone();
    }
    let proj = state.projections.lock().await;
    let (_response, events, _) = rpc::dispatch_request(
        json!({"jsonrpc": "2.0", "method": "channel.update", "params": params, "id": 1}),
        &proj,
        &state.channels_dir,
    );
    drop(proj);
    // Apply events to projections + broadcast + persist via daemon
    if !events.is_empty() {
        {
            let mut proj = state.projections.lock().await;
            for event in &events {
                proj.apply(event);
                let _ = state.event_tx.send(event.clone());
            }
        }
        let persist = crate::daemon_v2::decisions::Command::PersistEvents(events);
        let _ = state.command_tx.send(persist).await;
    }
    Json(json!({"ok": true}))
}

pub async fn channel_agents_md(
    State(state): State<Arc<WebState>>,
    axum::extract::Path(channel): axum::extract::Path<String>,
) -> Json<Value> {
    let path = state
        .channels_dir
        .join("channels")
        .join(&channel)
        .join("AGENTS.md");
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    Json(json!({"content": content, "channel": channel}))
}

pub async fn channel_agents_md_put(
    State(state): State<Arc<WebState>>,
    axum::extract::Path(channel): axum::extract::Path<String>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let dir = state.channels_dir.join("channels").join(&channel);
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("AGENTS.md");
    let content = body.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let _ = std::fs::write(&path, content);
    Json(json!({"ok": true}))
}

pub async fn channel_directory_get(
    State(state): State<Arc<WebState>>,
    axum::extract::Path(channel): axum::extract::Path<String>,
) -> Json<Value> {
    let proj = state.projections.lock().await;
    let directory = proj
        .channels
        .channel_directory(&channel)
        .map(|d| json!(d))
        .unwrap_or(json!(null));
    Json(json!({"directory": directory, "channel": channel}))
}

pub async fn channel_directory_put(
    State(state): State<Arc<WebState>>,
    axum::extract::Path(channel): axum::extract::Path<String>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let directory = body
        .get("directory")
        .and_then(|v| v.as_str())
        .map(String::from);
    let event = crate::daemon_v2::events::DomainEvent::ChannelDirectorySet {
        channel: channel.clone(),
        directory,
    };
    {
        let mut proj = state.projections.lock().await;
        proj.apply(&event);
        let _ = state.event_tx.send(event.clone());
    }
    let persist = crate::daemon_v2::decisions::Command::PersistEvents(vec![event]);
    let _ = state.command_tx.send(persist).await;
    Json(json!({"ok": true}))
}

pub async fn channel_archive(
    State(state): State<Arc<WebState>>,
    axum::extract::Path(channel): axum::extract::Path<String>,
) -> (StatusCode, Json<Value>) {
    // Archive by renaming with .archived suffix (matches Channel::list convention)
    let ch_dir = state.channels_dir.join("channels").join(&channel);
    let archived_dir = state
        .channels_dir
        .join("channels")
        .join(format!("{channel}.archived"));
    if ch_dir.exists()
        && let Err(e) = std::fs::rename(&ch_dir, &archived_dir)
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("failed to archive: {e}")})),
        );
    }
    (StatusCode::OK, Json(json!({"ok": true})))
}

pub async fn channel_unarchive(
    State(state): State<Arc<WebState>>,
    axum::extract::Path(channel): axum::extract::Path<String>,
) -> (StatusCode, Json<Value>) {
    let archived_dir = state
        .channels_dir
        .join("channels")
        .join(format!("{channel}.archived"));
    let ch_dir = state.channels_dir.join("channels").join(&channel);
    if archived_dir.exists()
        && let Err(e) = std::fs::rename(&archived_dir, &ch_dir)
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("failed to unarchive: {e}")})),
        );
    }
    (StatusCode::OK, Json(json!({"ok": true})))
}

pub async fn directories() -> Json<Value> {
    // List repo subdirectories — stub
    Json(json!([]))
}

pub async fn push_vapid_key() -> (StatusCode, Json<Value>) {
    match crate::push::PushManager::new() {
        Ok(pm) => match pm.vapid_public_key_base64() {
            Ok(key) => (
                StatusCode::OK,
                Json(json!({"vapid_key": key, "publicKey": key})),
            ),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("{e}")})),
            ),
        },
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": format!("{e}")})),
        ),
    }
}

#[derive(Deserialize)]
pub struct PushSubscribeBody {
    endpoint: String,
    #[serde(default)]
    p256dh: Option<String>,
    #[serde(default)]
    auth: Option<String>,
    #[serde(default)]
    keys: Option<Value>,
}

pub async fn push_subscribe(Json(body): Json<PushSubscribeBody>) -> (StatusCode, Json<Value>) {
    let p256dh = body
        .p256dh
        .or_else(|| {
            body.keys
                .as_ref()
                .and_then(|k| k.get("p256dh"))
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .unwrap_or_default();
    let auth = body
        .auth
        .or_else(|| {
            body.keys
                .as_ref()
                .and_then(|k| k.get("auth"))
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .unwrap_or_default();

    match crate::push::PushManager::new() {
        Ok(pm) => {
            let sub = crate::push::PushSubscription {
                endpoint: body.endpoint.clone(),
                p256dh: p256dh.clone(),
                auth: auth.clone(),
            };
            if let Err(e) = pm.add_subscription(sub) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("{e}")})),
                );
            }
            (StatusCode::OK, Json(json!({"ok": true})))
        }
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": format!("{e}")})),
        ),
    }
}

pub async fn push_unsubscribe(Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    let endpoint = body.get("endpoint").and_then(|v| v.as_str()).unwrap_or("");
    match crate::push::PushManager::new() {
        Ok(pm) => {
            let _ = pm.remove_subscription(endpoint);
            (StatusCode::OK, Json(json!({"ok": true})))
        }
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": format!("{e}")})),
        ),
    }
}

pub async fn upload(mut multipart: axum::extract::Multipart) -> (StatusCode, Json<Value>) {
    let upload_dir = crate::paths::midtown_base_dir().join("uploads");
    let _ = std::fs::create_dir_all(&upload_dir);

    if let Ok(Some(field)) = multipart.next_field().await {
        let filename = field
            .file_name()
            .map(|f| f.to_string())
            .unwrap_or_else(|| format!("upload-{}", uuid::Uuid::new_v4()));
        // Sanitize: extract just the filename component, stripping any path traversal
        let safe_name = std::path::Path::new(&filename)
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| format!("upload-{}", uuid::Uuid::new_v4()));
        let path = upload_dir.join(&safe_name);

        match field.bytes().await {
            Ok(data) => {
                if let Err(e) = std::fs::write(&path, &data) {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error": format!("write failed: {e}")})),
                    );
                }
                return (
                    StatusCode::OK,
                    Json(json!({
                        "ok": true,
                        "filename": safe_name,
                        "path": path.to_string_lossy(),
                        "size": data.len(),
                    })),
                );
            }
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": format!("read failed: {e}")})),
                );
            }
        }
    }

    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": "no file in request"})),
    )
}

pub async fn upload_get(
    axum::extract::Path(filename): axum::extract::Path<String>,
) -> Result<axum::response::Response, StatusCode> {
    let upload_dir = crate::paths::midtown_base_dir().join("uploads");
    let path = upload_dir.join(&filename);
    if !path.exists() {
        return Err(StatusCode::NOT_FOUND);
    }
    let body = std::fs::read(&path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(axum::response::Response::builder()
        .header("content-type", "application/octet-stream")
        .body(axum::body::Body::from(body))
        .unwrap())
}

pub async fn auth_switch(Json(body): Json<Value>) -> Json<Value> {
    let profile = body
        .get("profile")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let provider_str = body
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("claude");
    let auth_provider = match provider_str {
        "codex" => crate::auth::AuthProvider::Codex,
        _ => crate::auth::AuthProvider::Claude,
    };
    tracing::info!(%profile, %provider_str, "auth switch requested");
    match crate::auth::set_current_profile_for(auth_provider, profile) {
        Ok(()) => Json(json!({"ok": true, "profile": profile, "provider": provider_str})),
        Err(e) => Json(json!({"ok": false, "error": format!("{e}")})),
    }
}

/// Start OAuth login flow.
///
/// Spawns `claude auth login` with `BROWSER=false` to suppress browser opening,
/// captures the manual OAuth URL from stdout, and holds the process for code
/// submission via stdin.
pub async fn auth_login(
    State(state): State<Arc<WebState>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let provider = body
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("claude");
    tracing::info!(%provider, "auth login requested — starting OAuth flow");

    let auth_provider = match provider {
        "codex" => crate::auth::AuthProvider::Codex,
        _ => crate::auth::AuthProvider::Claude,
    };
    let config_dir = crate::auth::current_profile_dir_for(auth_provider);
    tracing::info!(config_dir = %config_dir.display(), "using auth profile dir");

    let mut child = match tokio::process::Command::new("claude")
        .args(["auth", "login"])
        .env("BROWSER", "false")
        .env(auth_provider.env_var(), &config_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return Json(json!({"ok": false, "error": format!("Failed to start auth: {e}")}));
        }
    };

    // Read stdout to find the OAuth URL
    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        use tokio::io::AsyncReadExt;
        let read_fut = async {
            let mut buf = [0u8; 4096];
            loop {
                match stdout.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        output.push_str(&String::from_utf8_lossy(&buf[..n]));
                        if output.contains("visit:") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        };
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), read_fut).await;
    }

    let url = output
        .lines()
        .find(|l| l.contains("visit:"))
        .and_then(|l| l.split("visit:").nth(1))
        .map(|u| u.trim().to_string());

    let Some(url) = url else {
        let _ = child.kill().await;
        return Json(
            json!({"ok": false, "error": "Failed to get OAuth URL from claude auth login"}),
        );
    };

    // Hold the child process (with stdin) for code submission
    let stdin = child.stdin.take();
    {
        let mut pending = state.pending_auth_login.lock().await;
        *pending = Some(AuthLoginProcess { child, stdin });
    }

    Json(json!({"ok": true, "url": url}))
}

/// Submit the OAuth code to the waiting `claude auth login` process via stdin.
///
/// The manual code from Claude's platform page is in format `CODE#STATE`.
/// Writing it to the CLI's stdin triggers `handleManualAuthCodeInput`, which
/// resolves the OAuth flow using the manual redirect_uri.
pub async fn auth_login_code(
    State(state): State<Arc<WebState>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let code = match body.get("code").and_then(|v| v.as_str()) {
        Some(c) if !c.trim().is_empty() => c.trim().to_string(),
        _ => return Json(json!({"ok": false, "error": "Missing code"})),
    };

    let mut pending = state.pending_auth_login.lock().await;
    let Some(mut process) = pending.take() else {
        return Json(json!({"ok": false, "error": "No pending auth login"}));
    };

    // Write code to stdin and close it
    if let Some(mut stdin) = process.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let write_result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            stdin.write_all(format!("{code}\n").as_bytes()).await?;
            stdin.flush().await?;
            drop(stdin); // Close stdin to signal EOF
            Ok::<(), std::io::Error>(())
        })
        .await;

        match write_result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                return Json(json!({"ok": false, "error": format!("Failed to send code: {e}")}));
            }
            Err(_) => {
                return Json(json!({"ok": false, "error": "Timed out writing code to stdin"}));
            }
        }
    }

    // Wait for process to complete — auth code submission should be fast
    let mut child = process.child;
    let auth_ok = match tokio::time::timeout(std::time::Duration::from_secs(30), child.wait()).await
    {
        Ok(Ok(status)) if status.success() => {
            tracing::info!("auth login completed successfully");
            true
        }
        Ok(Ok(status)) => {
            return Json(json!({"ok": false, "error": format!("auth login exited with {status}")}));
        }
        Ok(Err(e)) => {
            return Json(json!({"ok": false, "error": format!("auth login error: {e}")}));
        }
        Err(_) => {
            let _ = child.kill().await;
            return Json(json!({"ok": false, "error": "auth login timed out after 30s"}));
        }
    };

    // After successful auth, restart all running agents so they pick up the new token.
    // Stop them — the scheduler will respawn leads, and task dispatch will respawn workers.
    if auth_ok {
        let proj = state.projections.lock().await;
        let running: Vec<String> = proj.agents.running.iter().cloned().collect();
        drop(proj);
        for agent_id in running {
            let cmd = crate::daemon_v2::decisions::Command::StopAgent {
                id: agent_id,
                reason: "restarting after auth refresh".into(),
            };
            if let Err(e) = state.command_tx.send(cmd).await {
                tracing::warn!(%e, "failed to send stop command after auth refresh");
            }
        }
        tracing::info!("stopped all running agents for auth refresh — scheduler will respawn");
    }

    Json(json!({"ok": true}))
}

pub struct AuthLoginProcess {
    pub child: tokio::process::Child,
    pub stdin: Option<tokio::process::ChildStdin>,
}

pub async fn webhook_handler(
    State(state): State<Arc<WebState>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> (StatusCode, Json<Value>) {
    let event_type = headers
        .get("x-github-event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");

    // Parse the webhook payload and convert to domain events
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "invalid JSON"})),
            );
        }
    };

    tracing::info!(%event_type, "received GitHub webhook");

    // Convert to domain events using the existing webhook converter
    // For now, handle the most common events directly
    let mut events = Vec::new();

    match event_type {
        "pull_request" => {
            let action = payload.get("action").and_then(|v| v.as_str()).unwrap_or("");
            let pr = &payload["pull_request"];
            let number = pr.get("number").and_then(|v| v.as_u64()).unwrap_or(0);
            let branch = pr
                .get("head")
                .and_then(|h| h.get("ref"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let github_author = pr
                .get("user")
                .and_then(|u| u.get("login"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            match action {
                "opened" | "ready_for_review" => {
                    events.push(crate::daemon_v2::events::DomainEvent::PrOpened {
                        number,
                        branch,
                        github_author,
                    });
                    events
                        .push(crate::daemon_v2::events::DomainEvent::PrReviewRequested { number });
                }
                "closed" => {
                    let merged = pr.get("merged").and_then(|v| v.as_bool()).unwrap_or(false);
                    if merged {
                        events.push(crate::daemon_v2::events::DomainEvent::PrMerged {
                            number,
                            branch,
                        });
                    } else {
                        events.push(crate::daemon_v2::events::DomainEvent::PrClosed { number });
                    }
                }
                _ => {}
            }
        }
        // Spec 3.1: handle review state changes
        "pull_request_review" => {
            let action = payload.get("action").and_then(|v| v.as_str()).unwrap_or("");
            if action == "submitted" {
                let pr = &payload["pull_request"];
                let number = pr.get("number").and_then(|v| v.as_u64()).unwrap_or(0);
                let state = payload
                    .get("review")
                    .and_then(|r| r.get("state"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let review_state = match state {
                    "approved" => crate::daemon_v2::events::ReviewState::Approved,
                    "changes_requested" => crate::daemon_v2::events::ReviewState::ChangesRequested,
                    _ => crate::daemon_v2::events::ReviewState::Pending,
                };

                if number > 0 {
                    events.push(crate::daemon_v2::events::DomainEvent::PrUpdated {
                        number,
                        ci_status: crate::daemon_v2::events::CiStatus::Passed, // CI unchanged
                        review_state,
                    });
                }
            }
        }
        // Spec 12: handle PR comments — both top-level (issue_comment on PR)
        // and inline review comments (pull_request_review_comment)
        "issue_comment" | "pull_request_review_comment" => {
            let action = payload.get("action").and_then(|v| v.as_str()).unwrap_or("");
            if action == "created" {
                let pr_number = if event_type == "issue_comment" {
                    // issue_comment: PR number is in issue.number (only if issue.pull_request exists)
                    payload
                        .get("issue")
                        .filter(|issue| issue.get("pull_request").is_some())
                        .and_then(|issue| issue.get("number"))
                        .and_then(|n| n.as_u64())
                } else {
                    // pull_request_review_comment: PR number is in pull_request.number
                    payload
                        .get("pull_request")
                        .and_then(|pr| pr.get("number"))
                        .and_then(|n| n.as_u64())
                };

                if let Some(pr_num) = pr_number {
                    let commenter = payload
                        .get("comment")
                        .and_then(|c| c.get("user"))
                        .and_then(|u| u.get("login"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let body = payload
                        .get("comment")
                        .and_then(|c| c.get("body"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    // Route the comment through the decision function
                    let proj = state.projections.lock().await;
                    let commands = crate::daemon_v2::decisions::prs::route_pr_comment(
                        &proj, pr_num, commenter, body,
                    );
                    drop(proj);

                    for cmd in commands {
                        if let Err(e) = state.command_tx.send(cmd).await {
                            tracing::warn!(%e, "failed to send PR comment command");
                        }
                    }
                }
            }
        }
        // Spec 3.1: handle check_run events for CI status changes
        "check_run" | "check_suite" => {
            // CI status changes are picked up by the polling backstop (diff_pr_state).
            // Webhook events here could be used for faster updates, but the polling
            // path already handles PrUpdated events. Log for observability.
            tracing::debug!(%event_type, "CI webhook received (handled by polling backstop)");
        }
        _ => {
            tracing::debug!(%event_type, "unhandled webhook event type");
        }
    }

    // Apply events to projections + broadcast + persist via daemon
    if !events.is_empty() {
        {
            let mut proj = state.projections.lock().await;
            for event in &events {
                proj.apply(event);
            }
            for event in &events {
                let _ = state.event_tx.send(event.clone());
            }
        }
        let persist = crate::daemon_v2::decisions::Command::PersistEvents(events.clone());
        let _ = state.command_tx.send(persist).await;
    }

    (
        StatusCode::OK,
        Json(json!({"ok": true, "events": events.len()})),
    )
}

pub async fn mark_read(
    State(state): State<Arc<WebState>>,
    axum::extract::Path((item_type, id)): axum::extract::Path<(String, String)>,
    Json(body): Json<Value>,
) -> StatusCode {
    let timestamp = body
        .get("timestamp")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if timestamp.is_empty() {
        return StatusCode::BAD_REQUEST;
    }

    let mut data = load_read_state(&state.channels_dir);
    let key = match item_type.as_str() {
        "channel" => "channels",
        "thread" => "threads",
        _ => return StatusCode::BAD_REQUEST,
    };
    if let Some(obj) = data.get_mut(key).and_then(|v| v.as_object_mut()) {
        obj.insert(id, Value::String(timestamp));
    }
    save_read_state(&state.channels_dir, &data);
    StatusCode::NO_CONTENT
}
