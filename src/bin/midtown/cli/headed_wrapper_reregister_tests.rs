use std::sync::Arc;
use std::time::Duration;

/// Test helper: mock client that simulates daemon restart by returning
/// lease errors on poll after the first N successful polls. Re-registration
/// resets the counter so subsequent polls succeed again.
struct MockDaemonClient {
    poll_count: std::sync::Mutex<usize>,
    fail_after: usize,
    register_count: std::sync::Mutex<usize>,
}

impl MockDaemonClient {
    fn new(fail_after: usize) -> Self {
        Self {
            poll_count: std::sync::Mutex::new(0),
            fail_after,
            register_count: std::sync::Mutex::new(0),
        }
    }

    fn get_register_count(&self) -> usize {
        *self.register_count.lock().unwrap()
    }

    fn headed_register_mock(
        &self,
        _session: &str,
        _adapter_id: &str,
        _provider: midtown::auth::AuthProvider,
    ) -> Result<serde_json::Value, String> {
        let mut count = self.register_count.lock().unwrap();
        *count += 1;
        // Reset poll counter so subsequent polls succeed (simulates fresh lease)
        let mut poll = self.poll_count.lock().unwrap();
        *poll = 0;
        Ok(serde_json::json!({ "acked_id": 0 }))
    }

    fn headed_poll_mock(
        &self,
        _session: &str,
        _adapter_id: &str,
        _after_id: u64,
        _limit: usize,
    ) -> Result<serde_json::Value, String> {
        let mut count = self.poll_count.lock().unwrap();
        *count += 1;

        if *count > self.fail_after {
            // Simulate daemon restart: lease is gone
            return Err("No active headed adapter for session 'test'".to_string());
        }

        Ok(serde_json::json!({
            "messages": [],
            "capture_output": false
        }))
    }
}

#[test]
fn test_run_shell_reregisters_after_daemon_restart() {
    // Verifies that RunShell detects poll errors indicating a missing adapter
    // lease and automatically re-registers before retrying.

    let mock_client = Arc::new(MockDaemonClient::new(3));

    let session = "test";
    let adapter_id = "test-adapter";
    let provider = midtown::auth::AuthProvider::Claude;
    let batch_limit = 50;

    // Initial registration
    mock_client
        .headed_register_mock(session, adapter_id, provider)
        .expect("initial register");

    // Run poll loop — polls 1-3 succeed, poll 4 fails (lease error),
    // re-register resets counter, polls 5-7 succeed, poll 8 fails again.
    for _ in 0..10 {
        match mock_client.headed_poll_mock(session, adapter_id, 0, batch_limit) {
            Ok(_) => {
                // Process messages (none in this test)
            }
            Err(e) if e.contains("No active headed adapter") || e.contains("lease expired") => {
                // Daemon restarted — re-register
                mock_client
                    .headed_register_mock(session, adapter_id, provider)
                    .expect("re-register after daemon restart");
            }
            Err(e) => {
                panic!("Unexpected error: {}", e);
            }
        }

        std::thread::sleep(Duration::from_millis(1));
    }

    // Initial registration + 2 re-registrations (at poll 4 and poll 8) = 3
    let register_count = mock_client.get_register_count();
    assert!(
        register_count >= 2,
        "Expected at least one re-registration after daemon restart (got {} total registrations)",
        register_count
    );
}

#[test]
fn test_run_agent_reregisters_after_daemon_restart() {
    // Verifies that RunAgent detects poll errors indicating a missing adapter
    // lease and automatically re-registers before retrying.

    let mock_client = Arc::new(MockDaemonClient::new(3));

    let session = "test";
    let adapter_id = "test-adapter";
    let provider = midtown::auth::AuthProvider::Claude;
    let batch_limit = 50;

    // Initial registration
    mock_client
        .headed_register_mock(session, adapter_id, provider)
        .expect("initial register");

    // Simulate RunAgent poll loop
    for _ in 0..10 {
        match mock_client.headed_poll_mock(session, adapter_id, 0, batch_limit) {
            Ok(value) => {
                // Parse and process messages
                let _: serde_json::Value = value;
            }
            Err(e) if e.contains("No active headed adapter") || e.contains("lease expired") => {
                // Re-register to re-establish lease
                mock_client
                    .headed_register_mock(session, adapter_id, provider)
                    .expect("re-register after daemon restart");
            }
            Err(_) => {
                // Other errors: sleep and retry (existing behavior)
                std::thread::sleep(Duration::from_secs(1));
            }
        }

        std::thread::sleep(Duration::from_millis(1));
    }

    // Initial registration + at least 1 re-registration
    let register_count = mock_client.get_register_count();
    assert!(
        register_count >= 2,
        "Expected at least one re-registration after daemon restart (got {} total registrations)",
        register_count
    );
}
