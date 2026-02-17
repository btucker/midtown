//! Midtown - Multi-agent workspace management daemon for Gas Town.
//!
//! This crate provides the core library for the Midtown daemon, which manages
//! multiple agent workspaces (polecats, refineries, witnesses) in a Git-based
//! workflow system.
//!
//! ## Core Components
//!
//! - **RPC**: JSON-RPC 2.0 protocol for inter-process communication
//! - **Channels**: Append-only message logs for agent coordination
//! - **Cursors**: Per-agent position tracking in message streams
//! - **Worktrees**: Git worktree isolation for coworkers
//! - **Coworkers**: Agent session management (headless sessions)
//!
//! ## Quick Start
//!
//! The primary abstractions are [`Channel`] for communication and [`Message`]
//! for individual messages:
//!
//! ```
//! # use tempfile::TempDir;
//! use midtown::{Channel, Message, MessageType};
//!
//! # let temp_dir = TempDir::new().unwrap();
//! // Create a channel for agent communication
//! let channel = Channel::new(temp_dir.path(), "midtown").unwrap();
//!
//! // Agents send messages to the channel
//! channel.send(&Message::text("lead", "Starting build")).unwrap();
//! channel.send(&Message::status("worker1", "Compiling...")).unwrap();
//! channel.send(&Message::text("worker1", "Build complete")).unwrap();
//!
//! // Each agent tracks their read position with cursors
//! let messages = channel.read_since_cursor("worker2").unwrap();
//! assert_eq!(messages.len(), 3);
//!
//! // Subsequent reads only return new messages
//! channel.send(&Message::text("lead", "Deploy now")).unwrap();
//! let new_messages = channel.read_since_cursor("worker2").unwrap();
//! assert_eq!(new_messages.len(), 1);
//! ```

// Daemon server
pub mod daemon;

// Pure decision functions and shared types for the daemon tick loop
pub mod rules;

// Pane content pattern detection (usage limits, API errors, UI chrome)
pub mod pane_detection;

// RPC subsystem (furiosa)
pub mod rpc;

// Worktree management for coworker isolation (slit)
pub mod worktree;

// Channel management subsystem (nux)
mod channel;
mod cursor;
mod message;

// Coworker management
pub mod coworker;

// Process management (orphan cleanup, PID tracking, Zellij detection)
pub mod process;

// Settings and prompt file management for Claude Code sessions
pub mod settings;

// GitHub webhook integration (rictus)
pub mod webhook;

// Web server for Svelte mobile app
pub mod web;

// Standalone multi-project webserver
pub mod webserver;

// Project configuration
pub mod config;

// Structured coworker state reporting (replaces pane-content parsing for decisions)
pub mod coworker_state;

// Agent system prompts
pub mod agents;

// Claude Code task storage integration
pub mod tasks;

// Path utilities (socket paths, repo detection)
pub mod paths;

// Auth profile management for multi-account support
pub mod auth;

// Persistent GitHub state (PR reviewer assignments)
pub mod github_state;

// GitHub API rate limit tracking
pub mod github_rate_limit;

// Task-based worktree registry
pub mod worktree_registry;
#[path = "worktree_registry_tests.rs"]
#[cfg(test)]
mod worktree_registry_tests;

// CI check duration statistics (for auto-retry of stale checks)
pub mod ci_stats;

// Reminder system (one-shot condition-based reminders)
pub mod reminders;

// Web Push notification support
pub mod push;

// Randomized daemon event messages
pub mod daemon_messages;

// Headless Claude Code executor (JSON streaming)
pub mod headless;

// Unified launch configuration for Claude Code sessions
pub mod launch;

// Lightweight filesystem sandbox (sandbox-exec on macOS, bwrap on Linux)
pub mod sandbox;

// Agent teams mailbox writer (filesystem-based message delivery)
pub mod mailbox;

// Session key type for multi-session coworker identity
pub mod session_key;

// Provider-specific adapters for headed (interactive) delivery paths
pub mod headed_adapter;

// Platform abstraction for CLI argument construction (shared by headed + headless)
pub mod platform;

// Platform-specific pre-launch hooks (shared by headed + headless launch paths)
pub mod platform_launch;

// API usage data (session + weekly utilization from Anthropic OAuth API)
pub mod usage;

// AI channel clustering for task organization
pub mod clustering;

// Specialized headless coworker abstraction
pub mod specialized;

// Test utilities
// Note: Always available for use in both library and binary tests.
// The retry_with_backoff function is small and has no dependencies,
// so there's no harm in including it in production builds.
pub mod test_utils;

pub use channel::{Channel, ChannelInfo, ChannelRouter};
pub use coworker::{Coworker, CoworkerManager, CoworkerStatus, is_coworker_name};
pub use cursor::Cursor;
pub use message::{Message, MessageType};
pub use session_key::SessionKey;
pub use usage::{UsageData, fetch_usage_for_profile};
pub use worktree::{WorktreeError, WorktreeInfo, WorktreeManager};

/// Resolve the `web-app/dist/` directory containing built static assets.
///
/// Checks candidates in order and returns the first that exists:
/// 1. Next to the running executable (`exe_dir/web-app/dist`)
/// 2. In the source tree where the binary was compiled (`CARGO_MANIFEST_DIR/web-app/dist`)
///
/// Falls back to the source-tree path even if it doesn't exist, so callers
/// get a meaningful path for error messages.
pub fn resolve_web_dir() -> std::path::PathBuf {
    // Candidate 1: next to the executable (works for bundled installs)
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
    {
        let candidate = exe_dir.join("web-app").join("dist");
        if candidate.exists() {
            return candidate;
        }
    }

    // Candidate 2: source tree where `cargo build` ran (baked in at compile time)
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("web-app")
        .join("dist")
}

use thiserror::Error;

/// Errors that can occur in the Midtown daemon.
#[derive(Error, Debug)]
pub enum Error {
    /// I/O error (file, socket, etc.)
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization/deserialization error
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// RPC protocol error
    #[error("RPC error: {message} (code: {code})")]
    Rpc { code: i32, message: String },

    /// Channel not found
    #[error("Channel not found: {0}")]
    ChannelNotFound(String),

    /// Invalid message format
    #[error("Invalid message format: {0}")]
    InvalidMessage(String),
}

/// Result type alias for Midtown operations.
pub type Result<T> = std::result::Result<T, Error>;
