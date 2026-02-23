use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::paths;
use crate::rpc::{Request, RequestId, Response};

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);

pub fn post_matrix_event_as_daemon(channel: &str, from: &str, body: &str) -> Result<(), String> {
    if channel.trim().is_empty() {
        return Err("channel cannot be empty".to_string());
    }
    if body.trim().is_empty() {
        return Ok(());
    }

    let socket_path = paths::daemon_socket();
    if !socket_path.exists() {
        return Err(format!(
            "Daemon socket not found at {}",
            socket_path.display()
        ));
    }

    let mut stream = UnixStream::connect(&socket_path)
        .map_err(|e| format!("Failed to connect to daemon socket: {e}"))?;

    stream
        .set_write_timeout(Some(std::time::Duration::from_secs(5)))
        .map_err(|e| format!("Failed to set daemon socket write timeout: {e}"))?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .map_err(|e| format!("Failed to set daemon socket read timeout: {e}"))?;

    let request_id = RequestId::from(format!(
        "matrix-as-{}",
        REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let request = Request::new(
        "channel.post",
        Some(serde_json::json!({
            "channel": channel,
            "from": from,
            "message": body,
        })),
        request_id,
    );

    let request = serde_json::to_string(&request)
        .map_err(|e| format!("Failed to serialize daemon request: {e}"))?;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .map_err(|e| format!("Failed to write daemon request: {e}"))?;
    stream
        .flush()
        .map_err(|e| format!("Failed to flush daemon request: {e}"))?;

    let mut reader = BufReader::new(stream);
    let mut response_line = String::new();
    reader
        .read_line(&mut response_line)
        .map_err(|e| format!("Failed to read daemon response: {e}"))?;

    let response: Response = serde_json::from_str(response_line.trim_end())
        .map_err(|e| format!("Failed to parse daemon response: {e}"))?;
    if let Some(error) = response.error {
        return Err(error.message);
    }

    Ok(())
}
