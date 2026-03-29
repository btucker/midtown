pub mod routes;
pub mod websocket;

use axum::Router;
use axum::routing::{get, post};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};

use crate::daemon_v2::events::DomainEvent;
use crate::daemon_v2::projections::Projections;

pub struct WebState {
    pub projections: Arc<Mutex<Projections>>,
    pub channels_dir: PathBuf,
    pub event_tx: broadcast::Sender<DomainEvent>,
}

pub fn create_router(state: Arc<WebState>) -> Router {
    Router::new()
        .route("/api/health", get(routes::health))
        .route("/api/status", get(routes::status))
        .route("/api/channels", get(routes::channel_list))
        .route("/api/channels/history", get(routes::channel_history))
        .route("/api/channels/create", post(routes::channel_create))
        .route("/api/read-state", get(routes::read_state))
        .route(
            "/api/read-state/{item_type}/{id}",
            axum::routing::put(routes::mark_read),
        )
        .route("/api/search", get(routes::search))
        .route(
            "/api/channels/{channel}/settings",
            get(routes::channel_settings_get).put(routes::channel_settings_put),
        )
        .route(
            "/api/channels/{channel}/agents-md",
            get(routes::channel_agents_md).put(routes::channel_agents_md_put),
        )
        .route(
            "/api/channels/{channel}/directory",
            get(routes::channel_directory_get).put(routes::channel_directory_put),
        )
        .route(
            "/api/channels/{channel}/archive",
            post(routes::channel_archive),
        )
        .route(
            "/api/channels/{channel}/unarchive",
            post(routes::channel_unarchive),
        )
        .route("/api/directories", get(routes::directories))
        .route("/api/usage", get(routes::usage))
        .route("/api/questions", get(routes::questions))
        .route("/api/auth/profiles", get(routes::auth_profiles))
        .route("/api/auth/switch", post(routes::auth_switch))
        .route("/api/auth/login", post(routes::auth_login))
        .route("/api/upload", post(routes::upload))
        .route("/api/uploads/{filename}", get(routes::upload_get))
        .route("/api/push/vapid-key", get(routes::push_vapid_key))
        .route("/api/push/subscribe", post(routes::push_subscribe))
        .route("/api/push/unsubscribe", post(routes::push_unsubscribe))
        .route("/api/ws", get(websocket::ws_handler))
        .with_state(state)
}
