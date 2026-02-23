use std::net::SocketAddr;
use std::path::PathBuf;

use axum::{
    Json, Router, extract::Path as AxumPath, extract::State, http::StatusCode,
    response::IntoResponse, routing::put,
};
use serde::Deserialize;
use serde_json::Value;

use crate::matrix_bridge::{inbound, state::MatrixBridgeState, sync::room_for_channel_name};

#[derive(Debug, Clone)]
pub struct MatrixApplicationServiceState {
    project_name: String,
    homeserver_domain: String,
    state_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct MatrixApplicationService;

#[derive(Debug, Clone, Deserialize)]
pub struct MatrixTransaction {
    pub events: Vec<MatrixEvent>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MatrixEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub sender: String,
    pub room_id: String,
    pub content: Value,
}

impl MatrixApplicationService {
    pub fn router(state: MatrixApplicationServiceState) -> Router {
        Router::new()
            .route(
                "/_matrix/app/v1/transactions/:txn_id",
                put(handle_transaction),
            )
            .with_state(state)
    }
}

pub fn run_as_server(
    as_port: u16,
    project_name: &str,
    homeserver_domain: &str,
    state_path: &std::path::Path,
) -> Result<(), String> {
    let state = MatrixApplicationServiceState {
        project_name: project_name.to_string(),
        homeserver_domain: homeserver_domain.to_string(),
        state_path: state_path.to_path_buf(),
    };

    let app = MatrixApplicationService::router(state);
    let addr = SocketAddr::from(([127, 0, 0, 1], as_port));
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| format!("Failed to create Matrix AS runtime: {e}"))?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| format!("Failed to bind AS server on {addr}: {e}"))?;
        axum::serve(listener, app)
            .await
            .map_err(|e| format!("Matrix AS server stopped: {e}"))?;
        Ok::<(), String>(())
    })
}

pub async fn handle_transaction(
    AxumPath(_txn_id): AxumPath<String>,
    State(state): State<MatrixApplicationServiceState>,
    Json(body): Json<MatrixTransaction>,
) -> impl IntoResponse {
    for event in body.events {
        if event.event_type.as_str() != "m.room.message" {
            continue;
        }
        if !is_real_user(&event.sender, &state) {
            continue;
        }
        if let Some(body) = extract_message_body(&event.content)
            && let Some(channel_name) = room_to_channel(&event.room_id, &state.state_path)
            && let Some(from) = matrix_user_localpart(&event.sender, &state.homeserver_domain)
            && let Err(e) = inbound::post_matrix_event_as_daemon(&channel_name, &from, &body)
        {
            eprintln!("Matrix AS failed to post event to channel '{channel_name}': {e}");
        }
    }
    StatusCode::OK
}

fn room_to_channel(room_id: &str, state_path: &std::path::Path) -> Option<String> {
    let state = MatrixBridgeState::load(state_path).ok()?;
    room_for_channel_name(&state, room_id).map(|channel_name| channel_name.to_string())
}

fn is_real_user(sender: &str, app_state: &MatrixApplicationServiceState) -> bool {
    if let Some(localpart) = matrix_user_localpart(sender, &app_state.homeserver_domain) {
        if localpart == app_state.project_name {
            return false;
        }
        if let Ok(state) = MatrixBridgeState::load(&app_state.state_path) {
            if state.users.keys().any(|username| username == &localpart) {
                return false;
            }
            if state.users.values().any(|user_id| user_id == sender) {
                return false;
            }
        }
        return true;
    }
    false
}

fn matrix_user_localpart(sender: &str, homeserver_domain: &str) -> Option<String> {
    let sender = sender.strip_prefix('@')?;
    let (localpart, domain) = sender.split_once(':')?;
    if domain != homeserver_domain {
        return None;
    }
    Some(localpart.to_string())
}

fn extract_message_body(content: &Value) -> Option<String> {
    let body = content.get("body")?.as_str()?.trim().to_string();
    if body.is_empty() {
        return None;
    }
    Some(body)
}
