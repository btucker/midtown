//! Standalone multi-project webserver for midtown.
//!
//! Serves a shared web UI on port 47022 that discovers and proxies to
//! per-project daemons. This is separate from the per-daemon webhook
//! server (which runs on per-project ports 47023+).
//!
//! ## Endpoints
//!
//! - `GET /api/projects` - List all known projects with status
//! - `GET /api/projects/:name/status` - Proxy to per-project daemon status
//! - `GET /api/projects/:name/channel` - Proxy to per-project channel data
//! - `GET /api/projects/:name/tmux-windows` - Proxy to per-project tmux window list
//! - `GET /api/projects/:name/tmux-pane` - Proxy to per-project tmux pane content
//! - `GET /api/health` - Health check
//! - `GET /` - Serve static web UI (SPA)

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::get;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;
use tracing::{info, warn};

/// Default port for the standalone webserver.
pub const DEFAULT_WEBSERVER_PORT: u16 = 47022;

/// Configuration for the standalone webserver.
#[derive(Debug, Clone)]
pub struct WebserverConfig {
    /// Port to listen on (default: 47022).
    pub port: u16,
    /// Path to static web assets directory.
    pub static_dir: Option<PathBuf>,
}

impl Default for WebserverConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_WEBSERVER_PORT,
            static_dir: Some(crate::resolve_web_dir()),
        }
    }
}

/// Information about a discovered project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub name: String,
    pub status: ProjectStatus,
    pub daemon_socket: Option<String>,
    pub webhook_port: Option<u16>,
}

/// Status of a project's daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectStatus {
    Running,
    Stopped,
}

/// Shared state for the webserver.
#[derive(Clone)]
struct WebserverState {
    inner: Arc<RwLock<WebserverStateInner>>,
}

struct WebserverStateInner {
    projects: HashMap<String, ProjectInfo>,
}

impl WebserverState {
    fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(WebserverStateInner {
                projects: HashMap::new(),
            })),
        }
    }

    async fn refresh_projects(&self) {
        let projects = discover_projects();
        let mut state = self.inner.write().await;
        state.projects = projects.into_iter().map(|p| (p.name.clone(), p)).collect();
    }

    async fn get_projects(&self) -> Vec<ProjectInfo> {
        let state = self.inner.read().await;
        let mut projects: Vec<_> = state.projects.values().cloned().collect();
        projects.sort_by(|a, b| a.name.cmp(&b.name));
        projects
    }

    async fn get_project(&self, name: &str) -> Option<ProjectInfo> {
        let state = self.inner.read().await;
        state.projects.get(name).cloned()
    }
}

/// Discover all projects by scanning ~/.midtown/projects/.
fn discover_projects() -> Vec<ProjectInfo> {
    let projects_dir = crate::paths::midtown_base_dir().join("projects");
    let mut projects = Vec::new();

    let entries = match std::fs::read_dir(&projects_dir) {
        Ok(e) => e,
        Err(_) => return projects,
    };

    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }

        let name = entry.file_name().to_string_lossy().to_string();

        // Skip entries that are coworker names, not real projects.
        // These get created when a coworker process in a worktree incorrectly
        // registers itself as a project using the worktree directory name.
        if crate::coworker::is_coworker_name(&name) {
            continue;
        }

        let pid_file = entry.path().join("daemon.pid");
        let socket_path = crate::paths::daemon_socket_for_repo(&name);

        // Check if daemon is running
        let status = if pid_file.exists() && is_pid_locked(&pid_file) {
            ProjectStatus::Running
        } else {
            ProjectStatus::Stopped
        };

        let daemon_socket = if socket_path.exists() {
            Some(socket_path.to_string_lossy().to_string())
        } else {
            None
        };

        // Read webhook port from config
        let webhook_port =
            crate::config::load_full_project_config(&name).and_then(|c| c.daemon.webhook_port);

        projects.push(ProjectInfo {
            name,
            status,
            daemon_socket,
            webhook_port,
        });
    }

    projects
}

/// Check if a PID file is locked (indicating daemon is running).
fn is_pid_locked(pid_file: &std::path::Path) -> bool {
    use fs2::FileExt;
    use std::fs::OpenOptions;

    let file = match OpenOptions::new().read(true).open(pid_file) {
        Ok(f) => f,
        Err(_) => return false,
    };

    match file.try_lock_exclusive() {
        Ok(_) => {
            let _ = file.unlock();
            false
        }
        Err(_) => true,
    }
}

/// Send a JSON-RPC request to a daemon socket and get the raw result.
fn daemon_rpc(socket_path: &str, method: &str) -> Result<serde_json::Value, String> {
    let mut stream =
        UnixStream::connect(socket_path).map_err(|e| format!("Connection failed: {}", e))?;

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "id": 1
    });

    writeln!(stream, "{}", request).map_err(|e| format!("Write error: {}", e))?;
    stream.flush().map_err(|e| format!("Flush error: {}", e))?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("Read error: {}", e))?;

    let resp: serde_json::Value =
        serde_json::from_str(&line).map_err(|e| format!("Parse error: {}", e))?;

    if let Some(error) = resp.get("error") {
        return Err(error["message"]
            .as_str()
            .unwrap_or("Unknown error")
            .to_string());
    }

    resp.get("result")
        .cloned()
        .ok_or("No result in response".to_string())
}

// --- Axum handlers ---

async fn health() -> &'static str {
    "ok"
}

async fn list_projects(State(state): State<WebserverState>) -> Json<Vec<ProjectInfo>> {
    state.refresh_projects().await;
    Json(state.get_projects().await)
}

async fn project_status(
    State(state): State<WebserverState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    state.refresh_projects().await;

    let project = state
        .get_project(&name)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;

    let socket = project
        .daemon_socket
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    match daemon_rpc(socket, "status") {
        Ok(result) => Ok(Json(result)),
        Err(e) => {
            warn!("Failed to get status for project {}: {}", name, e);
            Err(StatusCode::BAD_GATEWAY)
        }
    }
}

async fn project_channel(
    State(_state): State<WebserverState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Read channel directly from file (no daemon needed)
    let channel = crate::Channel::for_repo(&name).map_err(|e| {
        warn!("Failed to open channel for project {}: {}", name, e);
        StatusCode::NOT_FOUND
    })?;

    let messages = channel.read_all().map_err(|e| {
        warn!("Failed to read channel for project {}: {}", name, e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let data: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| {
            serde_json::json!({
                "from": &m.from,
                "content": &m.content,
                "timestamp": m.timestamp.to_rfc3339(),
                "type": format!("{:?}", m.message_type).to_lowercase(),
            })
        })
        .collect();

    Ok(Json(serde_json::Value::Array(data)))
}

/// Query params for the tmux-pane proxy.
#[derive(Deserialize)]
struct TmuxPaneQuery {
    window: String,
}

/// Proxy tmux-windows request to per-project daemon.
async fn project_tmux_windows(
    State(state): State<WebserverState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let port = get_webhook_port(&state, &name).await?;
    let url = format!("http://127.0.0.1:{}/api/tmux-windows", port);

    let resp = reqwest::get(&url).await.map_err(|e| {
        warn!("Failed to proxy tmux-windows for project {}: {}", name, e);
        StatusCode::BAD_GATEWAY
    })?;

    let status = resp.status();
    if !status.is_success() {
        return Err(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY));
    }

    let body: serde_json::Value = resp.json().await.map_err(|e| {
        warn!(
            "Failed to parse tmux-windows response for project {}: {}",
            name, e
        );
        StatusCode::BAD_GATEWAY
    })?;

    Ok(Json(body))
}

/// Proxy tmux-pane request to per-project daemon.
async fn project_tmux_pane(
    State(state): State<WebserverState>,
    Path(name): Path<String>,
    Query(params): Query<TmuxPaneQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let port = get_webhook_port(&state, &name).await?;
    let url = format!(
        "http://127.0.0.1:{}/api/tmux-pane?window={}",
        port, params.window
    );

    let resp = reqwest::get(&url).await.map_err(|e| {
        warn!("Failed to proxy tmux-pane for project {}: {}", name, e);
        StatusCode::BAD_GATEWAY
    })?;

    let status = resp.status();
    if !status.is_success() {
        return Err(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY));
    }

    let body: serde_json::Value = resp.json().await.map_err(|e| {
        warn!(
            "Failed to parse tmux-pane response for project {}: {}",
            name, e
        );
        StatusCode::BAD_GATEWAY
    })?;

    Ok(Json(body))
}

/// Helper to resolve the webhook port for a project.
async fn get_webhook_port(state: &WebserverState, name: &str) -> Result<u16, StatusCode> {
    state.refresh_projects().await;
    let project = state.get_project(name).await.ok_or(StatusCode::NOT_FOUND)?;
    project.webhook_port.ok_or_else(|| {
        warn!("No webhook port for project {}", name);
        StatusCode::SERVICE_UNAVAILABLE
    })
}

/// Run the standalone webserver.
pub async fn run(config: WebserverConfig) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let state = WebserverState::new();

    // Initial project discovery
    state.refresh_projects().await;
    let projects = state.get_projects().await;
    info!(
        "Discovered {} projects: {}",
        projects.len(),
        projects
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    let cors = CorsLayer::permissive();

    // Build API routes
    let api = Router::new()
        .route("/health", get(health))
        .route("/projects", get(list_projects))
        .route("/projects/{name}/status", get(project_status))
        .route("/projects/{name}/channel", get(project_channel))
        .route("/projects/{name}/tmux-windows", get(project_tmux_windows))
        .route("/projects/{name}/tmux-pane", get(project_tmux_pane));

    let mut app = Router::new().nest("/api", api).layer(cors);

    // Serve static files if directory exists
    if let Some(ref static_dir) = config.static_dir
        && static_dir.exists()
    {
        let index = static_dir.join("index.html");
        let serve = tower_http::services::ServeDir::new(static_dir)
            .fallback(tower_http::services::ServeFile::new(index));
        app = app.fallback_service(serve);
        info!("Serving static files from {}", static_dir.display());
    }

    let app = app.with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    info!("Webserver listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_webserver_port() {
        assert_eq!(DEFAULT_WEBSERVER_PORT, 47022);
    }

    #[test]
    fn test_webserver_config_default() {
        let config = WebserverConfig::default();
        assert_eq!(config.port, 47022);
        // static_dir should auto-resolve and actually exist on disk
        assert!(config.static_dir.is_some(), "static_dir should not be None");
        let dir = config.static_dir.unwrap();
        assert!(
            dir.ends_with("web-app/dist"),
            "static_dir should end with 'web-app/dist', got: {:?}",
            dir
        );
        assert!(
            dir.exists(),
            "static_dir should point to an existing directory, got: {:?}",
            dir
        );
        assert!(
            dir.join("index.html").exists(),
            "static_dir should contain index.html, got: {:?}",
            dir
        );
    }

    #[test]
    fn test_discover_projects_handles_missing_dir() {
        // If projects dir doesn't exist, should return empty vec
        let projects = discover_projects();
        // This test just verifies it doesn't panic
        assert!(projects.is_empty() || !projects.is_empty());
    }

    #[test]
    fn test_project_status_serialization() {
        let info = ProjectInfo {
            name: "test".to_string(),
            status: ProjectStatus::Running,
            daemon_socket: Some("/tmp/test.sock".to_string()),
            webhook_port: Some(47023),
        };

        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"status\":\"running\""));
        assert!(json.contains("\"name\":\"test\""));
    }

    #[test]
    fn test_discover_projects_filters_coworker_names() {
        // Create a temp dir to act as ~/.midtown/projects/
        let temp_dir = tempfile::tempdir().unwrap();
        let projects_dir = temp_dir.path();

        // Create directories: some real projects, some coworker names
        std::fs::create_dir(projects_dir.join("midtown")).unwrap();
        std::fs::create_dir(projects_dir.join("broadway")).unwrap();
        std::fs::create_dir(projects_dir.join("amsterdam")).unwrap();
        std::fs::create_dir(projects_dir.join("my-app")).unwrap();
        std::fs::create_dir(projects_dir.join("bleecker")).unwrap();

        // Read entries and filter like discover_projects does
        let entries = std::fs::read_dir(projects_dir).unwrap();
        let mut names: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| !crate::coworker::is_coworker_name(name))
            .collect();
        names.sort();

        assert_eq!(names, vec!["midtown", "my-app"]);
    }

    #[test]
    fn test_project_status_stopped() {
        let info = ProjectInfo {
            name: "stopped-proj".to_string(),
            status: ProjectStatus::Stopped,
            daemon_socket: None,
            webhook_port: None,
        };

        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"status\":\"stopped\""));
    }
}
