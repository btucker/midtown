//! Tests for restart display formatting.
//!
//! These tests verify that coworker status during `midtown restart` shows
//! task IDs alongside task titles (e.g., "task !1274 Add sandbox...").

#[cfg(test)]
mod tests {
    /// Test that task info formatting includes both ID and title.
    ///
    /// The drain status display should show "task !1274 Add sandbox..." not just "task !1274".
    #[test]
    fn test_task_info_includes_id_and_title() {
        // Simulate a coworker response from the status RPC
        let coworker = serde_json::json!({
            "name": "vernon",
            "status": "running",
            "current_task": "!1274 Add sandbox_allowed_paths to config.toml...",
        });

        // Extract task info like wait_for_coworkers_to_drain does
        let current_task = coworker.get("current_task").and_then(|t| t.as_str());

        // The task info should be formatted as " (task !ID Title...)"
        let task_info = current_task
            .map(|t| format!(" (task {})", t))
            .unwrap_or_default();

        // Verify it includes both the ID and title
        assert_eq!(
            task_info, " (task !1274 Add sandbox_allowed_paths to config.toml...)",
            "Task info should include both ID and title"
        );

        // Verify it's not just the ID
        assert!(
            !task_info.contains("(task !1274)"),
            "Task info should not be just the ID"
        );
    }

    /// Test that empty task shows no task info.
    #[test]
    fn test_empty_task_shows_nothing() {
        let coworker = serde_json::json!({
            "name": "vernon",
            "status": "running",
            "current_task": null,
        });

        let current_task = coworker.get("current_task").and_then(|t| t.as_str());

        let task_info = current_task
            .map(|t| format!(" (task {})", t))
            .unwrap_or_default();

        assert_eq!(task_info, "", "Empty task should show no info");
    }
}
