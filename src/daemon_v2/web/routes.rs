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
    Json(response.get("result").cloned().unwrap_or(json!([])))
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
    Json(json!({}))
}

pub async fn questions() -> Json<Value> {
    Json(json!([]))
}

pub async fn auth_profiles() -> Json<Value> {
    Json(json!([]))
}
