//! Midtown daemon entry point.
//!
//! The daemon listens on a Unix socket and handles JSON-RPC requests for
//! workspace management operations.

use std::path::PathBuf;

use clap::Parser;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use midtown::rpc::{Request, Response, RpcError};
use midtown::WorktreeManager;

/// Midtown daemon - multi-agent workspace manager.
#[derive(Parser, Debug)]
#[command(name = "midtownd", version, about)]
struct Args {
    /// Path to the Unix socket
    #[arg(short, long, default_value = "/tmp/midtown.sock")]
    socket: PathBuf,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,
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

    // Remove existing socket file if present
    if args.socket.exists() {
        std::fs::remove_file(&args.socket)?;
    }

    // Bind to Unix socket
    let listener = UnixListener::bind(&args.socket)?;
    info!("Listening on {}", args.socket.display());

    // Set up shutdown signal handler
    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

    // Main accept loop
    loop {
        let shutdown_rx = shutdown_tx.subscribe();

        tokio::select! {
            // Accept new connections
            result = listener.accept() => {
                match result {
                    Ok((stream, _addr)) => {
                        debug!("New connection");
                        tokio::spawn(handle_connection(stream, shutdown_rx));
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

    // Clean up socket file
    if args.socket.exists() {
        std::fs::remove_file(&args.socket)?;
    }

    info!("Daemon stopped");
    Ok(())
}

/// Handle a single client connection.
async fn handle_connection(
    stream: tokio::net::UnixStream,
    mut shutdown_rx: broadcast::Receiver<()>,
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
                        let response = handle_request(&line);
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
fn handle_request(line: &str) -> Response {
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
        "ping" => {
            Response::success(request.id, serde_json::json!("pong"))
        }
        "version" => {
            Response::success(request.id, serde_json::json!({
                "name": "midtownd",
                "version": env!("CARGO_PKG_VERSION"),
            }))
        }
        "shutdown" => {
            info!("Shutdown requested via RPC");
            Response::success(request.id, serde_json::json!({"status": "shutting_down"}))
        }
        "coworker.spawn" => handle_coworker_spawn(&request),
        "coworker.shutdown" => handle_coworker_shutdown(&request),
        "coworker.list" => handle_coworker_list(&request),
        _ => {
            warn!("Unknown method: {}", request.method);
            Response::error(request.id, RpcError::method_not_found())
        }
    }
}

/// Handle coworker.spawn RPC request.
///
/// Creates a new worktree for a coworker and returns the coworker info.
fn handle_coworker_spawn(request: &Request) -> Response {
    // Extract coworker name from params, or generate one
    let name = request
        .params
        .as_ref()
        .and_then(|p| p.get("name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(generate_coworker_name);

    info!("Spawning coworker: {}", name);

    // Create the worktree manager
    let manager = match WorktreeManager::from_current_dir() {
        Ok(m) => m,
        Err(e) => {
            error!("Failed to create worktree manager: {}", e);
            return Response::error(
                request.id.clone(),
                RpcError::with_data(-32000, "Failed to detect repository", serde_json::json!({
                    "error": e.to_string()
                })),
            );
        }
    };

    // Create the worktree
    match manager.create(&name) {
        Ok(worktree_path) => {
            info!("Created worktree at: {}", worktree_path.display());
            Response::success(
                request.id.clone(),
                serde_json::json!({
                    "name": name,
                    "worktree_path": worktree_path.to_string_lossy(),
                    "branch": manager.branch_name(&name),
                    "repo": manager.repo_name(),
                }),
            )
        }
        Err(e) => {
            error!("Failed to create worktree: {}", e);
            Response::error(
                request.id.clone(),
                RpcError::with_data(-32001, "Failed to create worktree", serde_json::json!({
                    "error": e.to_string()
                })),
            )
        }
    }
}

/// Handle coworker.shutdown RPC request.
///
/// Removes the coworker's worktree and cleans up the branch if merged.
fn handle_coworker_shutdown(request: &Request) -> Response {
    // Extract coworker name from params (required)
    let name = match request
        .params
        .as_ref()
        .and_then(|p| p.get("name"))
        .and_then(|v| v.as_str())
    {
        Some(n) => n,
        None => {
            return Response::error(
                request.id.clone(),
                RpcError::invalid_params(),
            );
        }
    };

    let force = request
        .params
        .as_ref()
        .and_then(|p| p.get("force"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    info!("Shutting down coworker: {} (force={})", name, force);

    // Create the worktree manager
    let manager = match WorktreeManager::from_current_dir() {
        Ok(m) => m,
        Err(e) => {
            error!("Failed to create worktree manager: {}", e);
            return Response::error(
                request.id.clone(),
                RpcError::with_data(-32000, "Failed to detect repository", serde_json::json!({
                    "error": e.to_string()
                })),
            );
        }
    };

    // Remove the worktree
    match manager.remove(name, force) {
        Ok(()) => {
            info!("Removed worktree for coworker: {}", name);
            Response::success(
                request.id.clone(),
                serde_json::json!({
                    "name": name,
                    "status": "removed",
                }),
            )
        }
        Err(e) => {
            error!("Failed to remove worktree: {}", e);
            Response::error(
                request.id.clone(),
                RpcError::with_data(-32002, "Failed to remove worktree", serde_json::json!({
                    "error": e.to_string()
                })),
            )
        }
    }
}

/// Handle coworker.list RPC request.
///
/// Lists all coworker worktrees managed by this daemon.
fn handle_coworker_list(request: &Request) -> Response {
    // Create the worktree manager
    let manager = match WorktreeManager::from_current_dir() {
        Ok(m) => m,
        Err(e) => {
            error!("Failed to create worktree manager: {}", e);
            return Response::error(
                request.id.clone(),
                RpcError::with_data(-32000, "Failed to detect repository", serde_json::json!({
                    "error": e.to_string()
                })),
            );
        }
    };

    // List all worktrees
    match manager.list() {
        Ok(worktrees) => {
            let coworkers: Vec<_> = worktrees
                .iter()
                .filter(|w| w.is_coworker)
                .map(|w| {
                    serde_json::json!({
                        "name": w.coworker_name,
                        "path": w.path.to_string_lossy(),
                        "branch": w.branch,
                    })
                })
                .collect();

            Response::success(
                request.id.clone(),
                serde_json::json!({
                    "coworkers": coworkers,
                    "count": coworkers.len(),
                }),
            )
        }
        Err(e) => {
            error!("Failed to list worktrees: {}", e);
            Response::error(
                request.id.clone(),
                RpcError::with_data(-32003, "Failed to list worktrees", serde_json::json!({
                    "error": e.to_string()
                })),
            )
        }
    }
}

/// Generate a unique coworker name.
fn generate_coworker_name() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    format!("coworker-{}", timestamp % 100000)
}
