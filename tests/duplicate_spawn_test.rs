//! Test for duplicate spawn prevention when a headless session already exists.
//!
//! # Bug Description (2026-02-07)
//!
//! When the daemon tries to call in a coworker to address PR feedback or CI
//! failures, it attempts to spawn them even if they already have a running
//! headless session. This causes an "RPC error: Headless session <name> already
//! exists (code: -32603)" error and logs "call-in failed" to the channel.
//!
//! ## Root Cause
//!
//! The `spawn_coworker` method in `daemon/mod.rs` calls `session_manager.spawn()`
//! without first checking if a session already exists. The SessionManager
//! correctly rejects duplicate spawns, but the error bubbles up and gets logged
//! as a failure instead of being handled gracefully.
//!
//! ## Expected Behavior
//!
//! When `spawn_coworker` is called for a coworker that already has a running
//! session, it should:
//! 1. Detect that the session exists using `session_manager.is_alive()`
//! 2. Send the initial_prompt as a nudge message instead of spawning
//! 3. Return success
//!
//! This matches the semantic intent: "make sure this coworker is working on this
//! task", whether that means spawning them fresh or nudging their existing session.

use std::sync::Arc;

/// Mock SessionManager for testing duplicate spawn detection.
///
/// This is a simplified version that tracks alive sessions and initial prompts
/// sent, without actually spawning processes.
struct MockSessionManager {
    alive: std::sync::RwLock<std::collections::HashSet<String>>,
    messages: std::sync::RwLock<Vec<(String, String)>>,
}

impl MockSessionManager {
    fn new() -> Self {
        Self {
            alive: std::sync::RwLock::new(std::collections::HashSet::new()),
            messages: std::sync::RwLock::new(Vec::new()),
        }
    }

    async fn is_alive(&self, name: &str) -> bool {
        self.alive.read().unwrap().contains(name)
    }

    async fn spawn(&self, name: &str) -> Result<(), String> {
        let mut alive = self.alive.write().unwrap();
        if alive.contains(name) {
            return Err(format!("Headless session '{}' already exists", name));
        }
        alive.insert(name.to_string());
        Ok(())
    }

    async fn send_message(&self, name: &str, message: &str) -> Result<(), String> {
        if !self.is_alive(name).await {
            return Err(format!("No headless session for '{}'", name));
        }
        self.messages
            .write()
            .unwrap()
            .push((name.to_string(), message.to_string()));
        Ok(())
    }

    fn get_messages(&self) -> Vec<(String, String)> {
        self.messages.read().unwrap().clone()
    }
}

/// Simulate the spawn_coworker logic with duplicate detection.
async fn spawn_coworker_with_duplicate_check(
    session_manager: &MockSessionManager,
    name: &str,
    initial_prompt: Option<&str>,
) -> Result<(), String> {
    // Check if a headless session already exists for this coworker.
    // If so, send the initial prompt as a nudge instead of trying to spawn.
    if session_manager.is_alive(name).await {
        if let Some(prompt) = initial_prompt {
            session_manager.send_message(name, prompt).await?;
        }
        return Ok(());
    }

    // Spawn the headless session
    session_manager.spawn(name).await?;

    // Send initial prompt if provided
    if let Some(prompt) = initial_prompt {
        session_manager.send_message(name, prompt).await?;
    }

    Ok(())
}

#[tokio::test]
async fn test_duplicate_spawn_sends_nudge_instead() {
    let session_manager = Arc::new(MockSessionManager::new());

    // First spawn: should succeed
    let result = spawn_coworker_with_duplicate_check(
        &session_manager,
        "lexington",
        Some("Your task: investigate test failure"),
    )
    .await;
    assert!(result.is_ok(), "First spawn should succeed");
    assert!(session_manager.is_alive("lexington").await);

    // Second spawn: should detect existing session and nudge instead
    let result = spawn_coworker_with_duplicate_check(
        &session_manager,
        "lexington",
        Some("CI failed on PR #744, please investigate"),
    )
    .await;
    assert!(
        result.is_ok(),
        "Second spawn should succeed by nudging existing session"
    );

    // Verify the messages sent
    let messages = session_manager.get_messages();
    assert_eq!(messages.len(), 2, "Should have sent two messages");
    assert_eq!(
        messages[0],
        (
            "lexington".to_string(),
            "Your task: investigate test failure".to_string()
        )
    );
    assert_eq!(
        messages[1],
        (
            "lexington".to_string(),
            "CI failed on PR #744, please investigate".to_string()
        )
    );
}

#[tokio::test]
async fn test_duplicate_spawn_without_prompt() {
    let session_manager = Arc::new(MockSessionManager::new());

    // First spawn with no prompt
    let result = spawn_coworker_with_duplicate_check(&session_manager, "madison", None).await;
    assert!(result.is_ok(), "First spawn should succeed");

    // Second spawn with no prompt
    let result = spawn_coworker_with_duplicate_check(&session_manager, "madison", None).await;
    assert!(
        result.is_ok(),
        "Second spawn should succeed (no-op, no nudge needed)"
    );

    // No messages should have been sent
    let messages = session_manager.get_messages();
    assert_eq!(messages.len(), 0, "No messages should be sent");
}

#[tokio::test]
async fn test_fresh_spawn_still_works() {
    let session_manager = Arc::new(MockSessionManager::new());

    // Fresh spawn should work normally
    let result =
        spawn_coworker_with_duplicate_check(&session_manager, "broadway", Some("Review PR #123"))
            .await;
    assert!(result.is_ok(), "Fresh spawn should succeed");
    assert!(session_manager.is_alive("broadway").await);

    let messages = session_manager.get_messages();
    assert_eq!(messages.len(), 1, "Should have sent initial prompt");
    assert_eq!(
        messages[0],
        ("broadway".to_string(), "Review PR #123".to_string())
    );
}
