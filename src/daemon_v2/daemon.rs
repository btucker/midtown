use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::daemon_v2::decisions::health;
use crate::daemon_v2::events::EventStore;
use crate::daemon_v2::executor;
use crate::daemon_v2::projections::Projections;
use crate::daemon_v2::rpc;
use crate::daemon_v2::scheduler::Scheduler;

/// Configuration for DaemonV2.
pub struct DaemonV2Config {
    /// The directory key (git repo name) used for path resolution.
    pub dir_key: String,
    /// Path to the Unix domain socket.
    pub socket_path: PathBuf,
    /// Directory for the event log and snapshots.
    pub events_dir: PathBuf,
    /// Default channel name (used for lead health checks).
    pub default_channel: String,
}

/// The v2 daemon: owns the event store, projections, and scheduler.
pub struct DaemonV2 {
    config: DaemonV2Config,
    store: EventStore,
    projections: Projections,
    scheduler: Scheduler,
}

/// Exit status returned by [`DaemonV2::run`].
#[derive(Debug, PartialEq, Eq)]
pub enum DaemonV2ExitStatus {
    Shutdown,
}

/// Wrapper matching `DecisionFn = fn(&Projections, &str) -> Vec<Command>`.
/// Ignores the channel argument; delegates to the channel-agnostic health check.
fn check_dead_workers_fn(
    proj: &Projections,
    _channel: &str,
) -> Vec<crate::daemon_v2::decisions::Command> {
    health::check_dead_workers(proj)
}

/// Wrapper matching `DecisionFn`.
fn ensure_leads_alive_fn(
    proj: &Projections,
    channel: &str,
) -> Vec<crate::daemon_v2::decisions::Command> {
    health::ensure_leads_alive(proj, channel)
}

impl DaemonV2 {
    /// Create a new DaemonV2, recovering state from the event store.
    pub fn new(config: DaemonV2Config) -> std::io::Result<Self> {
        let (store, snapshot, replay_events) = EventStore::recover(config.events_dir.clone())?;

        let mut projections = snapshot.unwrap_or_default();
        projections.apply_all(&replay_events);

        let mut scheduler = Scheduler::new();
        scheduler.register(
            "check_dead_workers",
            Duration::from_secs(30),
            check_dead_workers_fn,
        );
        scheduler.register(
            "ensure_leads_alive",
            Duration::from_secs(30),
            ensure_leads_alive_fn,
        );

        Ok(Self {
            config,
            store,
            projections,
            scheduler,
        })
    }

    /// Run the event loop. Drives the Unix socket listener and the scheduler.
    /// Returns when a shutdown request is received.
    pub async fn run(mut self) -> DaemonV2ExitStatus {
        // Remove a stale socket file if it exists.
        let _ = std::fs::remove_file(&self.config.socket_path);

        let listener =
            UnixListener::bind(&self.config.socket_path).expect("failed to bind daemon socket");

        tracing::info!(socket = %self.config.socket_path.display(), "DaemonV2 listening");

        loop {
            let deadline = self
                .scheduler
                .next_deadline(Instant::now())
                .unwrap_or(Duration::from_secs(30));

            let sleep = tokio::time::sleep(deadline);
            tokio::pin!(sleep);

            tokio::select! {
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, _addr)) => {
                            let outcome = handle_rpc_connection(stream, &self.projections).await;
                            if outcome == RpcOutcome::Shutdown {
                                tracing::info!("shutdown requested via RPC");
                                return DaemonV2ExitStatus::Shutdown;
                            }
                        }
                        Err(e) => {
                            tracing::error!(%e, "accept error");
                        }
                    }
                }

                () = &mut sleep => {
                    self.run_due_decisions().await;
                }
            }
        }
    }

    /// Run all currently due decisions, execute the resulting commands, and
    /// apply the produced events to the event store and projections.
    async fn run_due_decisions(&mut self) {
        let now = Instant::now();
        let due = self.scheduler.due_decisions(now);

        for decision in due {
            let commands = (decision.run)(&self.projections, &self.config.default_channel);
            self.scheduler.mark_ran(decision.name, now);

            for command in commands {
                let events = executor::execute(command, &self.config.dir_key).await;
                for event in &events {
                    if let Err(e) = self.store.append(event) {
                        tracing::error!(%e, "failed to append event");
                    }
                    self.projections.apply(event);
                }
            }
        }
    }
}

// ── RPC connection handling ────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
enum RpcOutcome {
    Continue,
    Shutdown,
}

/// Read one JSON-RPC request from `stream`, dispatch it, and write the response.
/// Returns `RpcOutcome::Shutdown` if the request was a shutdown request.
async fn handle_rpc_connection(mut stream: UnixStream, proj: &Projections) -> RpcOutcome {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];

    // Read until the connection closes or we have a complete JSON object.
    loop {
        match stream.read(&mut tmp).await {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                // Attempt a parse; if it succeeds we have a complete request.
                if serde_json::from_slice::<serde_json::Value>(&buf).is_ok() {
                    break;
                }
            }
            Err(e) => {
                tracing::warn!(%e, "RPC read error");
                return RpcOutcome::Continue;
            }
        }
    }

    let request: serde_json::Value = match serde_json::from_slice(&buf) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(%e, "malformed RPC request");
            return RpcOutcome::Continue;
        }
    };

    // Check for the special "shutdown" method before dispatching.
    let is_shutdown = request.get("method").and_then(|m| m.as_str()) == Some("shutdown");

    if is_shutdown {
        let id = request
            .get("id")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "result": { "ok": true },
            "id": id
        });
        let _ = write_response(&mut stream, &response).await;
        return RpcOutcome::Shutdown;
    }

    let response = rpc::dispatch_request(request, proj);
    let _ = write_response(&mut stream, &response).await;

    RpcOutcome::Continue
}

async fn write_response(
    stream: &mut UnixStream,
    response: &serde_json::Value,
) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(response)?;
    stream.write_all(&bytes).await?;
    stream.shutdown().await?;
    Ok(())
}
