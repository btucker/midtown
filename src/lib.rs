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
//! - **Coworkers**: Agent session management via tmux
//! - **Tmux**: Low-level tmux session operations
//! - **Nudge**: Periodic and event-driven nudging for coworkers
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
//! let channel = Channel::new(temp_dir.path()).unwrap();
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

// RPC subsystem (furiosa)
pub mod rpc;

// Worktree management for coworker isolation (slit)
pub mod worktree;

// Channel management subsystem (nux)
mod channel;
mod cursor;
mod message;

// Coworker management (nux)
pub mod coworker;
pub mod tmux;

// GitHub webhook integration (rictus)
pub mod webhook;

// Agent nudging subsystem (dementus)
pub mod nudge;

pub use channel::Channel;
pub use coworker::{Coworker, CoworkerManager, CoworkerStatus};
pub use cursor::Cursor;
pub use message::{Message, MessageType};
pub use worktree::{WorktreeError, WorktreeInfo, WorktreeManager};

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

    /// Nudge operation failed
    #[error("Nudge error: {0}")]
    Nudge(#[from] nudge::NudgeError),
}

/// Result type alias for Midtown operations.
pub type Result<T> = std::result::Result<T, Error>;
