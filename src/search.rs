//! Full-text search across channel message history.
//!
//! Provides search across all JSONL channel logs using `rg` (ripgrep) for speed,
//! with a pure-Rust `regex` fallback when `rg` is not available.

use crate::channel::Channel;
use crate::message::Message;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::OnceLock;

/// Cached availability of `rg` binary in PATH.
static RG_AVAILABLE: OnceLock<bool> = OnceLock::new();

/// Check whether `rg` is available, caching the result.
fn is_rg_available() -> bool {
    *RG_AVAILABLE.get_or_init(|| {
        std::process::Command::new("rg")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// A single search result with context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub id: String,
    pub from: String,
    pub content: String,
    pub timestamp: String,
    pub channel: String,
    #[serde(rename = "type")]
    pub message_type: String,
    pub snippet: String,
}

/// Response from a search query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    pub query: String,
    pub total: usize,
}

/// Build a snippet of ~100 characters around the first match of `query` in `content`.
pub fn build_snippet(content: &str, query: &str, context_chars: usize) -> String {
    let lower_content = content.to_lowercase();
    let lower_query = query.to_lowercase();

    let pos = match lower_content.find(&lower_query) {
        Some(p) => p,
        None => return truncate_str(content, context_chars * 2),
    };

    let start = pos.saturating_sub(context_chars);
    let end = (pos + query.len() + context_chars).min(content.len());

    // Snap to char boundaries
    let start = snap_to_char_boundary(content, start, false);
    let end = snap_to_char_boundary(content, end, true);

    let mut snippet = String::new();
    if start > 0 {
        snippet.push_str("...");
    }
    snippet.push_str(&content[start..end]);
    if end < content.len() {
        snippet.push_str("...");
    }
    snippet
}

/// Snap a byte offset to the nearest valid char boundary.
/// If `forward` is true, snap forward; otherwise snap backward.
fn snap_to_char_boundary(s: &str, offset: usize, forward: bool) -> usize {
    if offset >= s.len() {
        return s.len();
    }
    if s.is_char_boundary(offset) {
        return offset;
    }
    if forward {
        (offset..=s.len())
            .find(|&i| s.is_char_boundary(i))
            .unwrap_or(s.len())
    } else {
        (0..=offset)
            .rev()
            .find(|&i| s.is_char_boundary(i))
            .unwrap_or(0)
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let end = snap_to_char_boundary(s, max_len, false);
        format!("{}...", &s[..end])
    }
}

/// Extract channel name from a JSONL file path.
///
/// Expected path pattern: `.../channels/<channel_name>/history/<file>.jsonl`
pub fn channel_name_from_path(path: &Path) -> Option<String> {
    // Walk up from the file: file.jsonl -> history -> <channel_name> -> channels
    let history_dir = path.parent()?;
    let channel_dir = history_dir.parent()?;
    let channels_dir = channel_dir.parent()?;

    if history_dir.file_name()?.to_str()? == "history"
        && channels_dir.file_name()?.to_str()? == "channels"
    {
        channel_dir
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
    } else {
        None
    }
}

/// Search messages across all channels using rg or regex fallback (async).
pub async fn search_messages(
    project_dir: &Path,
    query: &str,
    limit: usize,
) -> Result<SearchResponse, String> {
    if query.trim().is_empty() {
        return Ok(SearchResponse {
            results: vec![],
            query: query.to_string(),
            total: 0,
        });
    }

    let project_dir = project_dir.to_path_buf();
    let query = query.to_string();

    tokio::task::spawn_blocking(move || search_messages_sync(&project_dir, &query, limit))
        .await
        .map_err(|e| format!("Search task failed: {}", e))?
}

/// Search messages across all channels (sync, for TUI use).
pub fn search_messages_sync(
    project_dir: &Path,
    query: &str,
    limit: usize,
) -> Result<SearchResponse, String> {
    if query.trim().is_empty() {
        return Ok(SearchResponse {
            results: vec![],
            query: query.to_string(),
            total: 0,
        });
    }

    let channels_dir = project_dir.join("channels");
    if !channels_dir.exists() {
        return Ok(SearchResponse {
            results: vec![],
            query: query.to_string(),
            total: 0,
        });
    }

    let results = if is_rg_available() {
        search_with_rg(&channels_dir, query)?
    } else {
        search_with_regex(project_dir, query)?
    };

    // Sort by timestamp descending (newest first)
    let mut results = results;
    results.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    let total = results.len();
    results.truncate(limit);

    Ok(SearchResponse {
        results,
        query: query.to_string(),
        total,
    })
}

/// Search using `rg --json`.
fn search_with_rg(channels_dir: &Path, query: &str) -> Result<Vec<SearchResult>, String> {
    let output = std::process::Command::new("rg")
        .args([
            "--json",
            "-i",
            "--glob",
            "*.jsonl",
            query,
            channels_dir.to_str().unwrap_or("."),
        ])
        .output()
        .map_err(|e| format!("Failed to run rg: {}", e))?;

    // rg exits with 1 when no matches found — that's not an error
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(format!(
            "rg failed with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut results = Vec::new();

    for line in stdout.lines() {
        let rg_msg: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Only process "match" type entries from rg --json output
        if rg_msg.get("type").and_then(|t| t.as_str()) != Some("match") {
            continue;
        }

        let data = match rg_msg.get("data") {
            Some(d) => d,
            None => continue,
        };

        // Get the matched line text
        let line_text = match data
            .get("lines")
            .and_then(|l| l.get("text"))
            .and_then(|t| t.as_str())
        {
            Some(t) => t.trim(),
            None => continue,
        };

        // Parse the JSONL line as a Message
        let msg: Message = match serde_json::from_str(line_text) {
            Ok(m) => m,
            Err(_) => continue,
        };

        // Post-filter: only include matches where the content field contains the query
        if !msg.content.to_lowercase().contains(&query.to_lowercase()) {
            continue;
        }

        // Extract channel from file path
        let file_path = data
            .get("path")
            .and_then(|p| p.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("");
        let channel = channel_name_from_path(Path::new(file_path))
            .or_else(|| msg.channel.clone())
            .unwrap_or_else(|| "unknown".to_string());

        let snippet = build_snippet(&msg.content, query, 50);

        results.push(SearchResult {
            id: msg.id,
            from: msg.from,
            content: msg.content,
            timestamp: msg.timestamp.to_rfc3339(),
            channel,
            message_type: format!("{:?}", msg.message_type).to_lowercase(),
            snippet,
        });
    }

    Ok(results)
}

/// Fallback search using the `regex` crate when `rg` is not available.
fn search_with_regex(project_dir: &Path, query: &str) -> Result<Vec<SearchResult>, String> {
    let pattern = regex::RegexBuilder::new(&regex::escape(query))
        .case_insensitive(true)
        .build()
        .map_err(|e| format!("Invalid search pattern: {}", e))?;

    let channels = Channel::list(project_dir, false, None)
        .map_err(|e| format!("Failed to list channels: {}", e))?;

    let mut results = Vec::new();

    for info in channels {
        let channel =
            Channel::new(project_dir, &info.name).map_err(|e| format!("Channel error: {}", e))?;

        let messages = channel
            .read_all()
            .map_err(|e| format!("Failed to read channel '{}': {}", info.name, e))?;

        for msg in messages {
            if pattern.is_match(&msg.content) {
                let snippet = build_snippet(&msg.content, query, 50);
                results.push(SearchResult {
                    id: msg.id,
                    from: msg.from,
                    content: msg.content,
                    timestamp: msg.timestamp.to_rfc3339(),
                    channel: info.name.clone(),
                    message_type: format!("{:?}", msg.message_type).to_lowercase(),
                    snippet,
                });
            }
        }
    }

    Ok(results)
}

#[path = "search_tests.rs"]
#[cfg(test)]
mod tests;
