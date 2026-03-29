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

pub async fn channel_list(State(state): State<Arc<WebState>>) -> Json<Value> {
    let proj = state.projections.lock().await;
    let (response, _, _) = rpc::dispatch_request(
        json!({"jsonrpc": "2.0", "method": "channel.list", "id": 1}),
        &proj,
        &state.channels_dir,
    );
    let channels = response.get("result").cloned().unwrap_or(json!([]));
    // Web UI expects { channels: [...] }, not a bare array
    Json(json!({ "channels": channels }))
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
    // Return usage data so the AccountPanel renders (and with it, the theme toggle)
    Json(json!({ "usage": [] }))
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

pub async fn mark_read(
    axum::extract::Path((_item_type, _id)): axum::extract::Path<(String, String)>,
) -> StatusCode {
    StatusCode::NO_CONTENT
}
