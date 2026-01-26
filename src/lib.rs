//! Midtown - Multi-agent workspace management daemon.
//!
//! This crate provides the core library for the Midtown daemon, which manages
//! multiple agent workspaces (polecats, refineries, witnesses) in a Git-based
//! workflow system.

pub mod rpc;

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
}

/// Result type alias for Midtown operations.
pub type Result<T> = std::result::Result<T, Error>;
