use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
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
    Json(response.get("result").cloned().unwrap_or(json!(null)))
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
}

pub async fn channel_history(
    State(state): State<Arc<WebState>>,
    Query(params): Query<ChannelHistoryParams>,
) -> Json<Value> {
    let channel = params.channel.as_deref().unwrap_or("midtown");
    let limit = params.limit.or(params.last).or(Some(100)); // Default to last 100 messages
    let proj = state.projections.lock().await;
    let (response, _, _) = rpc::dispatch_request(
        json!({
            "jsonrpc": "2.0",
            "method": "channel.read",
            "params": {"channel": channel, "limit": limit},
            "id": 1
        }),
        &proj,
        &state.channels_dir,
    );
    let messages = response.get("result").cloned().unwrap_or(json!([]));

    // Filter by thread_parent_id if provided
    let messages = if let Some(ref parent_id) = params.thread_parent_id {
        match messages {
            Value::Array(arr) => Value::Array(
                arr.into_iter()
                    .filter(|msg| {
                        msg.get("thread_parent_id")
                            .and_then(|v| v.as_str())
                            .is_some_and(|id| id == parent_id)
                    })
                    .collect(),
            ),
            other => other,
        }
    } else {
        messages
    };

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
    let channels_dir = &state.channels_dir;
    match crate::daemon_v2::executor::channel_io::post_system_message(
        channels_dir,
        &body.name,
        "Channel created",
    ) {
        Ok(()) => (
            StatusCode::CREATED,
            Json(json!({"ok": true, "name": body.name})),
        ),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))),
    }
}

// Stub routes for endpoints the web UI calls but v2 doesn't fully implement yet.
// Return empty/default data instead of 404 so the UI doesn't break.

pub async fn read_state() -> Json<Value> {
    Json(json!({}))
}

pub async fn usage() -> Json<Value> {
    // Return usage data for active auth profiles.
    // Full usage stats (session_util, week_util) require API calls to Anthropic —
    // for now return profile info so the AccountPanel renders correctly.
    let mut usage = Vec::new();

    for provider in &[
        crate::auth::AuthProvider::Claude,
        crate::auth::AuthProvider::Codex,
    ] {
        let profile = crate::auth::current_profile_for(*provider);
        let profile_dir = crate::auth::current_profile_dir_for(*provider);
        if profile_dir.exists() {
            usage.push(json!({
                "provider": provider.as_str(),
                "profile": profile,
                "account_email": profile,
                "session_util": null,
                "session_resets": null,
                "week_util": null,
                "week_resets": null,
            }));
        }
    }

    Json(json!({ "usage": usage }))
}

pub async fn questions() -> Json<Value> {
    Json(json!([]))
}

pub async fn auth_profiles(Query(params): Query<AuthProfilesParams>) -> Json<Value> {
    // Return current auth profile so AccountPanel renders
    let provider = params.provider.as_deref().unwrap_or("claude");
    Json(json!([{
        "name": "default",
        "is_current": true,
        "has_credentials": true,
        "provider": provider,
    }]))
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
        .unwrap_or_else(|| json!({"show_full_lead_output": false, "lead_driven": false}));
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
    // Apply the events (channel settings changes)
    if !events.is_empty() {
        let mut proj = state.projections.lock().await;
        for event in &events {
            proj.apply(event);
        }
    }
    Json(json!({"ok": true}))
}

pub async fn channel_agents_md(
    axum::extract::Path(channel): axum::extract::Path<String>,
) -> Json<Value> {
    // Read AGENTS.md for a channel — stub for now
    Json(json!({"content": "", "channel": channel}))
}

pub async fn channel_directory_get(
    axum::extract::Path(channel): axum::extract::Path<String>,
) -> Json<Value> {
    Json(json!({"directory": null, "channel": channel}))
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
        // Sanitize filename
        let safe_name = filename.replace(['/', '\\'], "_").replace("..", "_");
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
                    Json(json!({"ok": true, "filename": safe_name, "size": data.len()})),
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
    let provider = body
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("claude");
    tracing::info!(%profile, %provider, "auth switch requested");
    Json(json!({"ok": true, "profile": profile, "provider": provider}))
}

pub async fn auth_login(Json(body): Json<Value>) -> Json<Value> {
    let email = body.get("email").and_then(|v| v.as_str()).unwrap_or("");
    tracing::info!(%email, "auth login requested");
    Json(json!({"ok": true}))
}

pub async fn mark_read(
    axum::extract::Path((_item_type, _id)): axum::extract::Path<(String, String)>,
) -> StatusCode {
    StatusCode::NO_CONTENT
}
