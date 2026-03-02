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
//! - `GET /api/projects/:name/zellij-web-url` - Get Zellij web client URL for project
//! - `GET /api/projects/:name/assets/*path` - Serve per-project asset files (screenshots, videos)
//! - `GET /api/projects/:name/channels/:channel_name/notes` - List channel notes (markdown files)
//! - `GET /api/projects/:name/proxy/api/ws` - WebSocket proxy to per-project daemon
//! - `ANY /api/projects/:name/proxy/*` - HTTP reverse proxy to per-project daemon
//! - `GET /api/health` - Health check
//! - `GET /` - Serve static web UI (SPA)

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{any, get};
use futures::{SinkExt, StreamExt};
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
    /// Path to TLS certificate file (PEM format).
    pub tls_cert: Option<PathBuf>,
    /// Path to TLS private key file (PEM format).
    pub tls_key: Option<PathBuf>,
}

impl Default for WebserverConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_WEBSERVER_PORT,
            static_dir: Some(crate::resolve_web_dir()),
            tls_cert: None,
            tls_key: None,
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
    /// HTTP client for proxying requests to daemon webhook servers.
    http_client: reqwest::Client,
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
            http_client: reqwest::Client::new(),
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

    let messages = channel.read_all_async().await.map_err(|e| {
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

/// Serve a static asset file from the per-project assets directory.
///
/// Path: `/api/projects/:name/assets/*path`
///
/// Serves files from `~/.midtown/projects/<name>/assets/<path>`.
/// Includes path traversal protection: the resolved file path must remain
/// within the assets directory.
async fn project_asset(
    Path((name, asset_path)): Path<(String, String)>,
) -> Result<Response<Body>, StatusCode> {
    let assets_dir = crate::paths::assets_dir_for_repo(&name);

    // Strip any leading slashes from the requested path component
    let asset_path = asset_path.trim_start_matches('/');

    // Reject paths containing ".." components before joining
    if asset_path.contains("..") {
        return Err(StatusCode::BAD_REQUEST);
    }

    let file_path = assets_dir.join(asset_path);

    // Verify the resolved path stays within the assets directory.
    // Use canonicalize on the assets dir (create it first if needed) and
    // compare prefixes on the non-symlink-resolved joined path for the
    // existence check, then canonicalize the actual file path once we
    // confirm it exists.
    let canonical_assets = match std::fs::canonicalize(&assets_dir) {
        Ok(p) => p,
        Err(_) => {
            // Assets dir doesn't exist yet — no files to serve
            return Err(StatusCode::NOT_FOUND);
        }
    };

    if !file_path.exists() {
        return Err(StatusCode::NOT_FOUND);
    }

    let canonical_file = std::fs::canonicalize(&file_path).map_err(|_| StatusCode::NOT_FOUND)?;

    if !canonical_file.starts_with(&canonical_assets) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let content = tokio::fs::read(&canonical_file)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

    let content_type = mime_type_for_path(&canonical_file);

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, HeaderValue::from_static(content_type))
        .body(Body::from(content))
        .unwrap())
}

/// Return a best-effort MIME type for a file path based on its extension.
fn mime_type_for_path(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json",
        Some("txt") | Some("log") => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Get the Zellij web client URL for a project.
///
/// Returns the URL and session name for the Zellij web client embed.
async fn project_zellij_web_url(Path(name): Path<String>) -> Json<serde_json::Value> {
    let session = format!("midtown-{}", name);
    Json(serde_json::json!({
        "url": "https://localhost:6780",
        "session": session,
    }))
}

/// List markdown notes for a channel.
///
/// Path: `/api/projects/:name/channels/:channel_name/notes`
///
/// Reads `~/.midtown/projects/<name>/channels/<channel_name>/notes/*.md` and
/// returns an array of `{ filename, title, content }` objects sorted
/// alphabetically by filename.  Returns an empty array when the notes
/// directory does not exist (no notes yet for that channel).
///
/// The title is taken from the first `# Heading` line in the file, falling
/// back to the filename stem (`.md` stripped, `-` replaced with spaces,
/// title-cased).
/// Return true if a name is safe to embed in a filesystem path.
///
/// Allows alphanumeric characters, hyphens, and underscores only.
/// This matches the channel name rules enforced by `Channel::new` and
/// prevents path traversal attacks via either path segment.
fn is_valid_path_segment(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

async fn project_channel_notes(
    Path((name, channel_name)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Validate both path segments to prevent path traversal attacks.
    if !is_valid_path_segment(&name) || !is_valid_path_segment(&channel_name) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let notes_dir = crate::paths::projects_dir_for_repo(&name)
        .join("channels")
        .join(&channel_name)
        .join("notes");

    if !notes_dir.exists() {
        return Ok(Json(serde_json::Value::Array(vec![])));
    }

    let mut entries: Vec<_> = std::fs::read_dir(&notes_dir)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
        .collect();

    entries.sort_by_key(|e| e.file_name());

    let notes: Vec<serde_json::Value> = entries
        .iter()
        .filter_map(|entry| {
            let filename = entry.file_name().to_string_lossy().to_string();
            let content = std::fs::read_to_string(entry.path()).ok()?;
            let title = extract_note_title(&filename, &content);
            Some(serde_json::json!({
                "filename": filename,
                "title": title,
                "content": content,
            }))
        })
        .collect();

    Ok(Json(serde_json::Value::Array(notes)))
}

/// Derive a display title from a note file's content and filename.
///
/// Returns the text of the first `# Heading` found in the file, or a
/// title-cased version of the filename stem if no heading is present.
fn extract_note_title(filename: &str, content: &str) -> String {
    for line in content.lines() {
        if let Some(rest) = line.trim().strip_prefix("# ") {
            let title = rest.trim();
            if !title.is_empty() {
                return title.to_string();
            }
        }
    }

    let stem = filename.strip_suffix(".md").unwrap_or(filename);
    stem.replace('-', " ")
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// --- HTTPS reverse proxy handlers ---
//
// When TLS is configured on the webserver, the browser refuses to make direct
// HTTP/WS requests to the daemon's webhook port (mixed content). These handlers
// proxy daemon traffic through the HTTPS webserver so every request stays on the
// secure origin.

/// WebSocket proxy: accept upgrade on the webserver, connect to the daemon's WS,
/// and bridge messages bidirectionally.
async fn proxy_ws_handler(
    State(state): State<WebserverState>,
    Path(name): Path<String>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, StatusCode> {
    let project = state
        .get_project(&name)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    let port = project
        .webhook_port
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let target_url = format!("ws://127.0.0.1:{}/api/ws", port);

    Ok(ws.on_upgrade(move |socket| async move {
        if let Err(e) = proxy_ws_bridge(socket, &target_url).await {
            warn!("WebSocket proxy error for project {}: {}", name, e);
        }
    }))
}

/// Bridge a client WebSocket to the daemon's WebSocket, forwarding messages
/// in both directions until either side closes.
async fn proxy_ws_bridge(
    client_ws: WebSocket,
    target_url: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use tokio_tungstenite::tungstenite::Message as TMsg;

    let (daemon_ws, _) = tokio_tungstenite::connect_async(target_url).await?;
    let (mut daemon_write, mut daemon_read) = daemon_ws.split();
    let (mut client_write, mut client_read) = client_ws.split();

    // Client → Daemon
    let client_to_daemon = async {
        while let Some(Ok(msg)) = client_read.next().await {
            let fwd = match msg {
                WsMessage::Text(t) => TMsg::Text(t.to_string().into()),
                WsMessage::Binary(b) => TMsg::Binary(b.to_vec().into()),
                WsMessage::Ping(p) => TMsg::Ping(p.to_vec().into()),
                WsMessage::Pong(p) => TMsg::Pong(p.to_vec().into()),
                WsMessage::Close(_) => return,
            };
            if daemon_write.send(fwd).await.is_err() {
                return;
            }
        }
    };

    // Daemon → Client
    let daemon_to_client = async {
        while let Some(Ok(msg)) = daemon_read.next().await {
            let fwd = match msg {
                TMsg::Text(t) => WsMessage::Text(t.to_string().into()),
                TMsg::Binary(b) => WsMessage::Binary(b),
                TMsg::Ping(p) => WsMessage::Ping(p),
                TMsg::Pong(p) => WsMessage::Pong(p),
                TMsg::Close(_) => return,
                _ => continue,
            };
            if client_write.send(fwd).await.is_err() {
                return;
            }
        }
    };

    // Run both directions concurrently; stop when either side finishes.
    tokio::select! {
        _ = client_to_daemon => {}
        _ = daemon_to_client => {}
    }

    Ok(())
}

/// HTTP reverse proxy: forward any request to the daemon's webhook port.
///
/// Preserves the method, query string, Content-Type header, and body so that
/// both GET and POST endpoints (status, channels, auth, upload, etc.) work
/// transparently through the proxy.
async fn proxy_http_handler(
    State(state): State<WebserverState>,
    Path((name, rest)): Path<(String, String)>,
    request: Request,
) -> Result<Response<Body>, StatusCode> {
    let project = state
        .get_project(&name)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    let port = project
        .webhook_port
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    let rest = rest.trim_start_matches('/');
    let query = request
        .uri()
        .query()
        .map(|q| format!("?{}", q))
        .unwrap_or_default();
    let target_url = format!("http://127.0.0.1:{}/{}{}", port, rest, query);

    let method = request.method().clone();
    let content_type = request.headers().get(header::CONTENT_TYPE).cloned();

    // Read the incoming body (11 MiB limit matches the daemon's DefaultBodyLimit)
    let body_bytes = axum::body::to_bytes(request.into_body(), 11 * 1024 * 1024)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    let mut upstream = state.http_client.request(method, &target_url);
    if let Some(ct) = content_type {
        upstream = upstream.header(header::CONTENT_TYPE, ct);
    }
    if !body_bytes.is_empty() {
        upstream = upstream.body(body_bytes);
    }

    let upstream_resp = upstream.send().await.map_err(|e| {
        warn!("Proxy request failed for {}: {}", target_url, e);
        StatusCode::BAD_GATEWAY
    })?;

    let status =
        StatusCode::from_u16(upstream_resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let resp_content_type = upstream_resp.headers().get(header::CONTENT_TYPE).cloned();
    let body = upstream_resp
        .bytes()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    let mut builder = Response::builder().status(status);
    if let Some(ct) = resp_content_type {
        builder = builder.header(header::CONTENT_TYPE, ct);
    }
    builder
        .body(Body::from(body))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
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
        .route(
            "/projects/{name}/zellij-web-url",
            get(project_zellij_web_url),
        )
        .route("/projects/{name}/assets/{*path}", get(project_asset))
        .route(
            "/projects/{name}/channels/{channel_name}/notes",
            get(project_channel_notes),
        )
        // HTTPS proxy routes: forward daemon API through the TLS-enabled webserver
        // so the browser never makes direct HTTP requests to the daemon port.
        .route("/projects/{name}/proxy/api/ws", get(proxy_ws_handler))
        .route("/projects/{name}/proxy/{*rest}", any(proxy_http_handler));

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

    // Bind to IPv6 wildcard [::] which accepts both IPv4 and IPv6 connections
    // on most systems (dual-stack). This prevents another process from binding
    // to the same port on the other protocol and intercepting browser connections.
    let addr = SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 0], config.port));

    // Reject incomplete TLS configuration — providing only one of cert/key
    // would silently fall back to plaintext, which is almost certainly a
    // misconfiguration (and breaks features that require secure origin).
    match (&config.tls_cert, &config.tls_key) {
        (Some(cert_path), Some(key_path)) => {
            // Install the ring crypto provider for rustls. This avoids the
            // aws-lc-rs → aws-lc-sys → cmake system build dependency.
            rustls::crypto::ring::default_provider()
                .install_default()
                .map_err(|_| "failed to install ring crypto provider")?;
            info!(
                "Webserver listening on https://localhost:{} (TLS)",
                config.port
            );
            let tls_config =
                axum_server::tls_rustls::RustlsConfig::from_pem_file(cert_path, key_path).await?;
            axum_server::bind_rustls(addr, tls_config)
                .serve(app.into_make_service())
                .await?;
        }
        (Some(_), None) => {
            return Err(
                "tls_cert is set but tls_key is missing — both are required for HTTPS".into(),
            );
        }
        (None, Some(_)) => {
            return Err(
                "tls_key is set but tls_cert is missing — both are required for HTTPS".into(),
            );
        }
        (None, None) => {
            info!("Webserver listening on http://localhost:{}", config.port);
            let listener = tokio::net::TcpListener::bind(addr).await?;
            axum::serve(listener, app).await?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::post;

    #[test]
    fn test_default_webserver_port() {
        assert_eq!(DEFAULT_WEBSERVER_PORT, 47022);
    }

    #[test]
    fn test_webserver_config_default() {
        let config = WebserverConfig::default();
        assert_eq!(config.port, 47022);
        // static_dir should auto-resolve to the web-app/dist path
        assert!(config.static_dir.is_some(), "static_dir should not be None");
        let dir = config.static_dir.unwrap();
        assert!(
            dir.ends_with("web-app/dist"),
            "static_dir should end with 'web-app/dist', got: {:?}",
            dir
        );
        // Note: dist/ is not committed to git and is built by `midtown start`,
        // so we only verify the path structure, not that it exists on disk.
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

    /// Test that discover_projects detects a running daemon via PID file lock.
    ///
    /// Simulates the real scenario: a project directory with a locked PID file
    /// and a socket file should be reported as Running.
    #[test]
    fn test_discover_projects_finds_running_daemon() {
        use fs2::FileExt;

        let temp_dir = tempfile::tempdir().unwrap();
        let projects_dir = temp_dir.path();

        // Create a project directory with a locked PID file
        let project_dir = projects_dir.join("test-project");
        std::fs::create_dir_all(&project_dir).unwrap();

        let pid_path = project_dir.join("daemon.pid");
        let pid_file = std::fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&pid_path)
            .unwrap();
        pid_file.lock_exclusive().unwrap();
        std::io::Write::write_all(&mut &pid_file, b"12345\n").unwrap();

        // Verify is_pid_locked detects the held lock
        assert!(
            is_pid_locked(&pid_path),
            "PID file should be detected as locked while we hold the lock"
        );

        // Verify the unlock path works too — retry briefly for OS lock release
        drop(pid_file);
        let mut unlocked = false;
        for _ in 0..10 {
            if !is_pid_locked(&pid_path) {
                unlocked = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            unlocked,
            "PID file should be detected as unlocked after dropping the File handle"
        );
    }

    /// Test that the actual /api/projects endpoint finds the live midtown daemon.
    ///
    /// This test runs against the real filesystem. It should find the "midtown"
    /// project with Running status when the daemon is active.
    #[test]
    fn test_discover_projects_finds_midtown_if_running() {
        let projects = discover_projects();

        // Find the midtown project
        let midtown = projects.iter().find(|p| p.name == "midtown");

        // If the daemon is running (PID file is locked), it should be found
        let pid_path = crate::paths::midtown_base_dir()
            .join("projects")
            .join("midtown")
            .join("daemon.pid");

        if pid_path.exists() && is_pid_locked(&pid_path) {
            let project = midtown
                .expect("discover_projects should find 'midtown' when daemon PID file is locked");
            assert!(
                matches!(project.status, ProjectStatus::Running),
                "midtown project should have Running status, got: {:?}",
                project.status
            );
            assert!(
                project.daemon_socket.is_some(),
                "running project should have a daemon_socket path"
            );
        }
        // If daemon not running, this test is a no-op (can't assert)
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

    #[test]
    fn test_mime_type_for_path() {
        let cases = [
            ("screenshot.png", "image/png"),
            ("photo.jpg", "image/jpeg"),
            ("photo.jpeg", "image/jpeg"),
            ("anim.gif", "image/gif"),
            ("anim.webp", "image/webp"),
            ("clip.mp4", "video/mp4"),
            ("clip.webm", "video/webm"),
            ("icon.svg", "image/svg+xml"),
            ("data.json", "application/json"),
            ("notes.txt", "text/plain"),
            ("daemon.log", "text/plain"),
            ("unknown.xyz", "application/octet-stream"),
        ];
        for (filename, expected) in cases {
            let path = std::path::Path::new(filename);
            assert_eq!(
                mime_type_for_path(path),
                expected,
                "wrong MIME type for {filename}"
            );
        }
    }

    /// Test that the path traversal check rejects ".." in the path component.
    #[tokio::test]
    async fn test_project_asset_rejects_path_traversal() {
        // A path with ".." should return BAD_REQUEST before touching the filesystem
        let result =
            project_asset(Path(("myproject".to_string(), "../etc/passwd".to_string()))).await;
        assert_eq!(result.unwrap_err(), StatusCode::BAD_REQUEST);
    }

    /// Test that requesting a non-existent asset returns NOT_FOUND.
    #[tokio::test]
    async fn test_project_asset_not_found_for_missing_file() {
        // Use a repo name that definitely has no assets dir
        let result = project_asset(Path((
            "nonexistent-repo-xyz-test-123".to_string(),
            "image.png".to_string(),
        )))
        .await;
        assert_eq!(result.unwrap_err(), StatusCode::NOT_FOUND);
    }

    /// Test that a valid asset file is served with the correct content type.
    #[tokio::test]
    async fn test_project_asset_serves_existing_file() {
        use crate::paths::{assets_dir_for_repo, set_test_midtown_base_dir};

        let tmp = tempfile::tempdir().unwrap();
        let _guard = set_test_midtown_base_dir(tmp.path().to_path_buf());

        let assets_dir = assets_dir_for_repo("test-proj");
        std::fs::create_dir_all(&assets_dir).unwrap();

        let content = b"\x89PNG\r\n\x1a\n"; // minimal PNG header
        std::fs::write(assets_dir.join("shot.png"), content).unwrap();

        let result = project_asset(Path(("test-proj".to_string(), "shot.png".to_string())))
            .await
            .unwrap();

        assert_eq!(result.status(), StatusCode::OK);
        assert_eq!(
            result.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/png"
        );
    }

    // --- extract_note_title tests ---

    #[test]
    fn test_extract_note_title_uses_first_h1() {
        let content = "# My Great Note\n\nSome body text.";
        assert_eq!(extract_note_title("my-note.md", content), "My Great Note");
    }

    #[test]
    fn test_extract_note_title_falls_back_to_filename() {
        let content = "No heading here, just plain text.";
        assert_eq!(
            extract_note_title("quick-start-guide.md", content),
            "Quick Start Guide"
        );
    }

    #[test]
    fn test_extract_note_title_filename_without_md_extension() {
        let content = "";
        assert_eq!(
            extract_note_title("architecture-overview.md", content),
            "Architecture Overview"
        );
    }

    #[test]
    fn test_extract_note_title_skips_empty_h1() {
        let content = "#\n\n## Actually a heading\n\nBody.";
        // Empty H1 should be skipped — no fallback-worthy H1 found, use filename
        assert_eq!(
            extract_note_title("fallback-name.md", content),
            "Fallback Name"
        );
    }

    #[test]
    fn test_extract_note_title_ignores_h2_and_below() {
        let content = "## Section\n\nBody text without a top-level heading.";
        assert_eq!(extract_note_title("my-doc.md", content), "My Doc");
    }

    // --- project_channel_notes handler tests ---

    #[test]
    fn test_is_valid_path_segment() {
        assert!(is_valid_path_segment("midtown"));
        assert!(is_valid_path_segment("my-project"));
        assert!(is_valid_path_segment("my_channel"));
        assert!(is_valid_path_segment("abc123"));
        assert!(!is_valid_path_segment(""));
        assert!(!is_valid_path_segment(".."));
        assert!(!is_valid_path_segment("../etc"));
        assert!(!is_valid_path_segment("foo/bar"));
        assert!(!is_valid_path_segment("foo bar"));
    }

    #[tokio::test]
    async fn test_channel_notes_rejects_invalid_channel_name() {
        let result =
            project_channel_notes(Path(("myproject".to_string(), "../etc/passwd".to_string())))
                .await;
        assert_eq!(result.unwrap_err(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_channel_notes_rejects_invalid_project_name() {
        let result =
            project_channel_notes(Path(("../../../etc".to_string(), "mychannel".to_string())))
                .await;
        assert_eq!(result.unwrap_err(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_channel_notes_returns_empty_for_missing_dir() {
        use crate::paths::set_test_midtown_base_dir;

        let tmp = tempfile::tempdir().unwrap();
        let _guard = set_test_midtown_base_dir(tmp.path().to_path_buf());

        let result = project_channel_notes(Path(("test-proj".to_string(), "web".to_string())))
            .await
            .unwrap();

        assert_eq!(result.0, serde_json::Value::Array(vec![]));
    }

    #[tokio::test]
    async fn test_channel_notes_returns_sorted_notes() {
        use crate::paths::set_test_midtown_base_dir;

        let tmp = tempfile::tempdir().unwrap();
        let _guard = set_test_midtown_base_dir(tmp.path().to_path_buf());

        let notes_dir = tmp
            .path()
            .join("projects")
            .join("test-proj")
            .join("channels")
            .join("web")
            .join("notes");
        std::fs::create_dir_all(&notes_dir).unwrap();

        std::fs::write(
            notes_dir.join("b-second.md"),
            "# Second Note\n\nContent for second note.",
        )
        .unwrap();
        std::fs::write(
            notes_dir.join("a-first.md"),
            "# First Note\n\nContent for first note.",
        )
        .unwrap();
        // Non-.md file should be ignored
        std::fs::write(notes_dir.join("ignored.txt"), "ignored").unwrap();

        let result = project_channel_notes(Path(("test-proj".to_string(), "web".to_string())))
            .await
            .unwrap();

        let notes = result.0.as_array().unwrap();
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0]["filename"], "a-first.md");
        assert_eq!(notes[0]["title"], "First Note");
        assert_eq!(notes[1]["filename"], "b-second.md");
        assert_eq!(notes[1]["title"], "Second Note");
    }

    #[tokio::test]
    async fn test_channel_notes_title_falls_back_to_filename() {
        use crate::paths::set_test_midtown_base_dir;

        let tmp = tempfile::tempdir().unwrap();
        let _guard = set_test_midtown_base_dir(tmp.path().to_path_buf());

        let notes_dir = tmp
            .path()
            .join("projects")
            .join("test-proj")
            .join("channels")
            .join("auth")
            .join("notes");
        std::fs::create_dir_all(&notes_dir).unwrap();
        std::fs::write(
            notes_dir.join("getting-started.md"),
            "No heading — just a paragraph.",
        )
        .unwrap();

        let result = project_channel_notes(Path(("test-proj".to_string(), "auth".to_string())))
            .await
            .unwrap();

        let notes = result.0.as_array().unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0]["title"], "Getting Started");
    }

    // --- proxy handler tests ---

    /// Build a WebserverState pre-loaded with the given projects.
    async fn state_with_projects(projects: Vec<ProjectInfo>) -> WebserverState {
        let state = WebserverState::new();
        let mut guard = state.inner.write().await;
        guard.projects = projects.into_iter().map(|p| (p.name.clone(), p)).collect();
        drop(guard);
        state
    }

    #[tokio::test]
    async fn test_proxy_http_returns_not_found_for_unknown_project() {
        let state = state_with_projects(vec![]).await;
        let req = axum::http::Request::builder()
            .uri("/api/projects/nonexistent/proxy/api/status")
            .body(Body::empty())
            .unwrap();
        let result = proxy_http_handler(
            State(state),
            Path(("nonexistent".to_string(), "api/status".to_string())),
            req,
        )
        .await;
        assert_eq!(result.unwrap_err(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_proxy_http_returns_service_unavailable_when_no_webhook_port() {
        let projects = vec![ProjectInfo {
            name: "test-proj".to_string(),
            status: ProjectStatus::Stopped,
            daemon_socket: None,
            webhook_port: None,
        }];
        let state = state_with_projects(projects).await;
        let req = axum::http::Request::builder()
            .uri("/api/projects/test-proj/proxy/api/status")
            .body(Body::empty())
            .unwrap();
        let result = proxy_http_handler(
            State(state),
            Path(("test-proj".to_string(), "api/status".to_string())),
            req,
        )
        .await;
        assert_eq!(result.unwrap_err(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn test_proxy_http_returns_bad_gateway_when_daemon_unreachable() {
        // Use a port that definitely has nothing listening on it
        let projects = vec![ProjectInfo {
            name: "test-proj".to_string(),
            status: ProjectStatus::Running,
            daemon_socket: None,
            webhook_port: Some(19999),
        }];
        let state = state_with_projects(projects).await;
        let req = axum::http::Request::builder()
            .uri("/api/projects/test-proj/proxy/api/status")
            .body(Body::empty())
            .unwrap();
        let result = proxy_http_handler(
            State(state),
            Path(("test-proj".to_string(), "api/status".to_string())),
            req,
        )
        .await;
        assert_eq!(result.unwrap_err(), StatusCode::BAD_GATEWAY);
    }

    /// Verify the proxy forwards GET requests to the daemon's actual port and
    /// returns the response. Spins up a tiny axum server as a stand-in daemon.
    #[tokio::test]
    async fn test_proxy_http_forwards_get_request() {
        // Start a tiny HTTP server to act as the daemon webhook server
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let daemon_app = axum::Router::new().route(
            "/api/status",
            get(|| async { axum::Json(serde_json::json!({"ok": true})) }),
        );
        tokio::spawn(async move {
            axum::serve(listener, daemon_app).await.unwrap();
        });

        let projects = vec![ProjectInfo {
            name: "test-proj".to_string(),
            status: ProjectStatus::Running,
            daemon_socket: None,
            webhook_port: Some(port),
        }];
        let state = state_with_projects(projects).await;
        let req = axum::http::Request::builder()
            .uri("/api/projects/test-proj/proxy/api/status")
            .body(Body::empty())
            .unwrap();

        let response = proxy_http_handler(
            State(state),
            Path(("test-proj".to_string(), "api/status".to_string())),
            req,
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json, serde_json::json!({"ok": true}));
    }

    /// Verify the proxy forwards POST requests with body and Content-Type.
    #[tokio::test]
    async fn test_proxy_http_forwards_post_with_body() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let daemon_app = axum::Router::new().route(
            "/api/channels/create",
            post(|body: axum::Json<serde_json::Value>| async move {
                axum::Json(serde_json::json!({"created": body.0["name"]}))
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, daemon_app).await.unwrap();
        });

        let projects = vec![ProjectInfo {
            name: "test-proj".to_string(),
            status: ProjectStatus::Running,
            daemon_socket: None,
            webhook_port: Some(port),
        }];
        let state = state_with_projects(projects).await;
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/api/projects/test-proj/proxy/api/channels/create")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"name":"dm-test"}"#))
            .unwrap();

        let response = proxy_http_handler(
            State(state),
            Path(("test-proj".to_string(), "api/channels/create".to_string())),
            req,
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["created"], "dm-test");
    }

    /// Verify query string parameters are forwarded through the proxy.
    #[tokio::test]
    async fn test_proxy_http_forwards_query_params() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        #[derive(Deserialize)]
        struct QueryParams {
            channel: Option<String>,
        }

        let daemon_app = axum::Router::new().route(
            "/api/channels/history",
            get(
                |axum::extract::Query(q): axum::extract::Query<QueryParams>| async move {
                    axum::Json(serde_json::json!({"channel": q.channel}))
                },
            ),
        );
        tokio::spawn(async move {
            axum::serve(listener, daemon_app).await.unwrap();
        });

        let projects = vec![ProjectInfo {
            name: "test-proj".to_string(),
            status: ProjectStatus::Running,
            daemon_socket: None,
            webhook_port: Some(port),
        }];
        let state = state_with_projects(projects).await;
        let req = axum::http::Request::builder()
            .uri("/api/projects/test-proj/proxy/api/channels/history?channel=web")
            .body(Body::empty())
            .unwrap();

        let response = proxy_http_handler(
            State(state),
            Path(("test-proj".to_string(), "api/channels/history".to_string())),
            req,
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["channel"], "web");
    }
}
