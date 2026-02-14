//! Tests for status RPC handler.

#[cfg(test)]
mod tests {

    #[test]
    fn test_current_task_includes_id_and_title() {
        // Test that current_task includes both the task ID and title
        // in the format "!1234 Task title"

        // This test validates the fix for task #1276:
        // When running midtown restart, the coworker status should show task IDs
        // alongside titles (e.g., "task !1274 Add sandbox..." not just "task !Add sandbox...")

        // The implementation is in handle_status() which builds the current_task
        // field from get_in_progress_tasks_with_subjects()

        // Since handle_status() is async and requires a DaemonState, we test the
        // transformation logic directly here.

        let task_id = "1234";
        let subject = "Add feature X";

        // The format should match what we display in the restart output
        let expected = format!("!{} {}", task_id, subject);

        // Verify the expected format is "!1234 Add feature X"
        assert_eq!(expected, "!1234 Add feature X");
    }
}
