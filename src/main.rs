//! Midtown daemon entry point.
//!
//! The daemon listens on a Unix socket and handles JSON-RPC requests for
//! workspace management operations.

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use midtown::coworker::CoworkerManager;
use midtown::rpc::{Request, Response, RpcError};
// WorktreeManager available for future worktree+session integration
#[allow(unused_imports)]
use midtown::WorktreeManager;

/// Midtown daemon - multi-agent workspace manager.
#[derive(Parser, Debug)]
#[command(name = "midtownd", version, about)]
struct Args {
    /// Path to the Unix socket
    #[arg(short, long)]
    socket: Option<PathBuf>,

    /// Working directory for spawned coworkers
    #[arg(short, long)]
    workdir: Option<PathBuf>,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,
}

/// Shared daemon state.
struct DaemonState {
    coworkers: CoworkerManager,
    socket_path: PathBuf,
}

impl DaemonState {
    fn new(socket_path: PathBuf, workdir: PathBuf) -> Self {
        Self {
            coworkers: CoworkerManager::new(workdir.to_string_lossy().to_string()),
            socket_path,
        }
    }
}

#[tokio::main]
async fn main() -> midtown::Result<()> {
    let args = Args::parse();

    // Initialize logging
    let filter = if args.verbose { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    // Determine socket path: CLI arg > XDG_STATE_HOME > ~/.local/state
    let socket_path = args.socket.unwrap_or_else(|| {
        let state_dir = std::env::var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".local")
                    .join("state")
            });
        state_dir.join("midtown").join("daemon.sock")
    });

    // Ensure parent directory exists
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Determine working directory for coworkers
    let workdir = args.workdir.unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    });

    // Create daemon state
    let state = Arc::new(DaemonState::new(socket_path.clone(), workdir));

    // Remove existing socket file if present
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }

    // Bind to Unix socket
    let listener = UnixListener::bind(&socket_path)?;
    info!("Listening on {}", socket_path.display());

    // Set up shutdown signal handler
    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

    // Main accept loop
    loop {
        let shutdown_rx = shutdown_tx.subscribe();
        let state = Arc::clone(&state);

        tokio::select! {
            // Accept new connections
            result = listener.accept() => {
                match result {
                    Ok((stream, _addr)) => {
                        debug!("New connection");
                        tokio::spawn(handle_connection(stream, shutdown_rx, state));
                    }
                    Err(e) => {
                        error!("Accept error: {}", e);
                    }
                }
            }

            // Handle SIGTERM
            _ = sigterm.recv() => {
                info!("Received SIGTERM, shutting down...");
                let _ = shutdown_tx.send(());
                break;
            }

            // Handle SIGINT (Ctrl+C)
            _ = sigint.recv() => {
                info!("Received SIGINT, shutting down...");
                let _ = shutdown_tx.send(());
                break;
            }
        }
    }

    // Shutdown all coworkers
    info!("Shutting down coworkers...");
    if let Err(e) = state.coworkers.shutdown_all() {
        warn!("Error shutting down coworkers: {}", e);
    }

    // Clean up socket file
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }

    info!("Daemon stopped");
    Ok(())
}

/// Handle a single client connection.
async fn handle_connection(
    stream: tokio::net::UnixStream,
    mut shutdown_rx: broadcast::Receiver<()>,
    state: Arc<DaemonState>,
) {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    loop {
        line.clear();

        tokio::select! {
            // Read next request line
            result = reader.read_line(&mut line) => {
                match result {
                    Ok(0) => {
                        debug!("Client disconnected");
                        break;
                    }
                    Ok(_) => {
                        let response = handle_request(&line, &state);
                        let response_json = match serde_json::to_string(&response) {
                            Ok(json) => json,
                            Err(e) => {
                                error!("Failed to serialize response: {}", e);
                                continue;
                            }
                        };

                        if let Err(e) = writer.write_all(response_json.as_bytes()).await {
                            warn!("Failed to write response: {}", e);
                            break;
                        }
                        if let Err(e) = writer.write_all(b"\n").await {
                            warn!("Failed to write newline: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        warn!("Read error: {}", e);
                        break;
                    }
                }
            }

            // Handle shutdown signal
            _ = shutdown_rx.recv() => {
                debug!("Connection handler received shutdown signal");
                break;
            }
        }
    }
}

/// Process a JSON-RPC request and return a response.
fn handle_request(line: &str, state: &DaemonState) -> Response {
    // Parse the request
    let request: Request = match serde_json::from_str(line) {
        Ok(req) => req,
        Err(e) => {
            warn!("Failed to parse request: {}", e);
            return Response::error(
                midtown::rpc::RequestId::Null,
                RpcError::parse_error(),
            );
        }
    };

    debug!("Received request: method={}", request.method);

    // Dispatch based on method
    match request.method.as_str() {
        "ping" => Response::success(request.id, serde_json::json!("pong")),

        "version" => Response::success(
            request.id,
            serde_json::json!({
                "name": "midtownd",
                "version": env!("CARGO_PKG_VERSION"),
            }),
        ),

        "shutdown" => {
            info!("Shutdown requested via RPC");
            Response::success(request.id, serde_json::json!({"status": "shutting_down"}))
        }

        "coworker.spawn" => handle_coworker_spawn(request.id, state),

        "coworker.shutdown" => {
            let name = request
                .params
                .as_ref()
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str());

            match name {
                Some(name) => handle_coworker_shutdown(request.id, name, state),
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "coworker.list" => handle_coworker_list(request.id, state),

        "coworker.nudge" => {
            let params = request.params.as_ref();
            let name = params.and_then(|p| p.get("name")).and_then(|v| v.as_str());
            let message = params
                .and_then(|p| p.get("message"))
                .and_then(|v| v.as_str());

            match (name, message) {
                (Some(name), Some(message)) => handle_coworker_nudge(request.id, name, message, state),
                _ => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "status" => handle_status(request.id, state),

        _ => {
            warn!("Unknown method: {}", request.method);
            Response::error(request.id, RpcError::method_not_found())
        }
    }
}

/// Handle coworker.spawn RPC method.
fn handle_coworker_spawn(id: midtown::rpc::RequestId, state: &DaemonState) -> Response {
    match state.coworkers.spawn() {
        Ok(name) => {
            info!("Spawned coworker: {}", name);
            Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Spawned coworker: {}", name),
                    "coworkers": [{
                        "name": name,
                        "status": "running",
                        "current_task": null,
                        "started_at": chrono::Utc::now().to_rfc3339(),
                    }]
                }),
            )
        }
        Err(e) => {
            error!("Failed to spawn coworker: {}", e);
            Response::error(id, RpcError::new(-32603, e.to_string()))
        }
    }
}

/// Handle coworker.shutdown RPC method.
fn handle_coworker_shutdown(
    id: midtown::rpc::RequestId,
    name: &str,
    state: &DaemonState,
) -> Response {
    match state.coworkers.shutdown(name) {
        Ok(()) => {
            info!("Shutdown coworker: {}", name);
            Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Shutdown coworker: {}", name),
                }),
            )
        }
        Err(e) => {
            error!("Failed to shutdown coworker {}: {}", name, e);
            Response::error(id, RpcError::new(-32603, e.to_string()))
        }
    }
}

/// Handle coworker.list RPC method.
fn handle_coworker_list(id: midtown::rpc::RequestId, state: &DaemonState) -> Response {
    let coworkers: Vec<serde_json::Value> = state
        .coworkers
        .list()
        .iter()
        .map(|cw| {
            serde_json::json!({
                "name": cw.name,
                "status": cw.status.to_string(),
                "current_task": cw.current_task,
                "started_at": cw.started_at.to_rfc3339(),
            })
        })
        .collect();

    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "coworkers": coworkers,
        }),
    )
}

/// Handle coworker.nudge RPC method.
fn handle_coworker_nudge(
    id: midtown::rpc::RequestId,
    name: &str,
    message: &str,
    state: &DaemonState,
) -> Response {
    match state.coworkers.nudge(name, message) {
        Ok(()) => {
            info!("Nudged coworker {}: {}", name, message);
            Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Nudged coworker: {}", name),
                }),
            )
        }
        Err(e) => {
            error!("Failed to nudge coworker {}: {}", name, e);
            Response::error(id, RpcError::new(-32603, e.to_string()))
        }
    }
}

/// Handle status RPC method.
fn handle_status(id: midtown::rpc::RequestId, state: &DaemonState) -> Response {
    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "daemon_running": true,
            "active_coworkers": state.coworkers.count(),
            "pending_tasks": 0,  // TODO: implement task tracking
            "socket_path": state.socket_path.to_string_lossy(),
        }),
    )
}
